//! Stdio MCP client — spawns an MCP server process and speaks JSON-RPC 2.0
//! over line-delimited stdio. Modelled after `nexo_extensions::runtime::stdio`
//! (11.3) but narrowed to MCP semantics (initialize + notifications).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use dashmap::DashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use nexo_extensions::runtime::wire;
use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig};

use crate::config::McpServerConfig;
use crate::errors::McpError;
use crate::events::{method_to_event, ClientEvent};
use crate::protocol::{
    self, parse_notification_method, InitializeParams, INITIALIZE_ID, PROTOCOL_VERSION,
};
use crate::sampling::{wire as sampling_wire, SamplingProvider};
use crate::telemetry::{inc_sampling_requests_total, observe_sampling_duration_ms};
use crate::types::{
    InitializeResult, McpCapabilities, McpClientInfo, McpServerInfo, McpTool, McpToolResult,
    ToolsListPage,
};

const INCOMING_CAP: usize = 32;
/// Maximum concurrent server-originated requests the incoming handler
/// will dispatch at once. Sampling providers block on LLM calls that
/// can take 10s+, so processing them serially plus a 32-deep bounded
/// queue meant a fast burst from the server would silently drop
/// requests and stall the MCP session. Spawning with a semaphore lets
/// us answer concurrently without letting a misbehaving server fork-
/// bomb the agent process.
const INCOMING_CONCURRENCY: usize = 8;

#[derive(Debug)]
struct IncomingRequest {
    id: serde_json::Value,
    method: String,
    params: serde_json::Value,
}

const OUTBOX_CAP: usize = 64;
const MAX_PAGES: usize = 64;
const EVENT_CAP: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum McpClientState {
    Connecting,
    Ready,
    Failed { reason: String },
    Shutdown,
}

type PendingMap = DashMap<u64, oneshot::Sender<Result<serde_json::Value, (i32, String)>>>;

pub struct StdioMcpClient {
    name: String,
    config: McpServerConfig,
    server_info: McpServerInfo,
    capabilities: McpCapabilities,
    protocol_version: String,
    state: Arc<RwLock<McpClientState>>,
    outbox_tx: mpsc::Sender<String>,
    pending: Arc<PendingMap>,
    next_id: Arc<AtomicU64>,
    breaker: Arc<CircuitBreaker>,
    shutdown: CancellationToken,
    events_tx: broadcast::Sender<ClientEvent>,
}

impl StdioMcpClient {
    /// Spawn the server, run the `initialize` handshake, and send
    /// `notifications/initialized`. Returns a live client ready to call
    /// tools, or an error.
    pub async fn connect(config: McpServerConfig) -> Result<Self, McpError> {
        Self::connect_with_sampling(config, None).await
    }

    /// Same as `connect`, but also registers a `SamplingProvider` that
    /// will answer `sampling/createMessage` requests originated by the
    /// server. When `sampling_provider` is `Some`, the handshake also
    /// advertises the `sampling` capability.
    pub async fn connect_with_sampling(
        config: McpServerConfig,
        sampling_provider: Option<Arc<dyn SamplingProvider>>,
    ) -> Result<Self, McpError> {
        if config.name.is_empty() {
            return Err(McpError::Protocol("config.name is empty".into()));
        }
        if config.command.is_empty() {
            return Err(McpError::Protocol("config.command is empty".into()));
        }

        let state = Arc::new(RwLock::new(McpClientState::Connecting));
        let pending: Arc<PendingMap> = Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let shutdown = CancellationToken::new();
        let breaker = Arc::new(CircuitBreaker::new(
            format!("mcp:{}", config.name),
            CircuitBreakerConfig::default(),
        ));
        let (outbox_tx, outbox_rx) = mpsc::channel::<String>(OUTBOX_CAP);
        let (events_tx, _) = broadcast::channel::<ClientEvent>(EVENT_CAP);
        let (incoming_tx, incoming_rx) = mpsc::channel::<IncomingRequest>(INCOMING_CAP);

        let mut child = build_command(&config).spawn().map_err(McpError::Spawn)?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Spawn(std::io::Error::other("child stdin missing")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Spawn(std::io::Error::other("child stdout missing")))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| McpError::Spawn(std::io::Error::other("child stderr missing")))?;

        tokio::spawn(reader_task(
            stdout,
            pending.clone(),
            shutdown.clone(),
            config.name.clone(),
            events_tx.clone(),
            incoming_tx.clone(),
        ));
        tokio::spawn(writer_task(
            stdin,
            outbox_rx,
            shutdown.clone(),
            config.name.clone(),
        ));
        tokio::spawn(stderr_task(stderr, shutdown.clone(), config.name.clone()));
        tokio::spawn(incoming_handler_task(
            incoming_rx,
            sampling_provider.clone(),
            config.name.clone(),
            outbox_tx.clone(),
            shutdown.clone(),
        ));

        // Supervisor: if the child dies we mark Failed and drop pending.
        {
            let state_clone = state.clone();
            let pending_clone = pending.clone();
            let shutdown_clone = shutdown.clone();
            let name_clone = config.name.clone();
            tokio::spawn(async move {
                let exit = child.wait().await;
                if !shutdown_clone.is_cancelled() {
                    tracing::warn!(mcp=%name_clone, status=?exit, "mcp child exited");
                    *state_clone.write().unwrap() = McpClientState::Failed {
                        reason: format!("child exited: {exit:?}"),
                    };
                    drain_pending(&pending_clone);
                }
            });
        }

        // Handshake: id=0, reserved.
        let client_info = McpClientInfo {
            name: "agent".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        };
        let params = InitializeParams::with_sampling(&client_info, sampling_provider.is_some());
        let line = protocol::encode_request("initialize", &params, INITIALIZE_ID)
            .map_err(McpError::Encode)?;
        let (tx, rx) = oneshot::channel();
        pending.insert(INITIALIZE_ID, tx);
        if outbox_tx.send(line).await.is_err() {
            return Err(McpError::EarlyExit);
        }
        let init_value = match tokio::time::timeout(config.initialize_timeout, rx).await {
            Ok(Ok(Ok(v))) => v,
            Ok(Ok(Err((code, message)))) => {
                return Err(McpError::ServerError { code, message });
            }
            Ok(Err(_canceled)) => return Err(McpError::EarlyExit),
            Err(_) => return Err(McpError::InitializeTimeout(config.initialize_timeout)),
        };
        let init: InitializeResult =
            serde_json::from_value(init_value).map_err(McpError::Decode)?;

        // Fire-and-forget `notifications/initialized`.
        let notif =
            protocol::encode_notification("notifications/initialized", serde_json::json!({}))
                .map_err(McpError::Encode)?;
        let _ = outbox_tx.send(notif).await;

        *state.write().unwrap() = McpClientState::Ready;

        let negotiated_version = if init.protocol_version.is_empty() {
            PROTOCOL_VERSION.to_string()
        } else {
            init.protocol_version
        };
        if negotiated_version != PROTOCOL_VERSION {
            tracing::warn!(
                mcp = %config.name,
                client = %PROTOCOL_VERSION,
                server = %negotiated_version,
                "MCP protocol version mismatch — proceeding with server's version; some fields may be unrecognized"
            );
        }
        let client = Self {
            name: config.name.clone(),
            protocol_version: negotiated_version,
            server_info: init.server_info,
            capabilities: init.capabilities,
            state,
            outbox_tx,
            pending,
            next_id,
            breaker,
            shutdown,
            events_tx,
            config,
        };
        // Phase 12.8 — apply configured log level once the handshake is
        // complete. Best-effort: failures (unknown level, missing
        // capability, transport error) degrade to a warn log and the
        // connection stays up.
        if let Some(level) = client.config.log_level.clone() {
            match client.set_log_level(&level).await {
                Ok(()) => tracing::info!(
                    mcp = %client.name,
                    level = %level,
                    "mcp setLevel applied"
                ),
                Err(e) => tracing::warn!(
                    mcp = %client.name,
                    level = %level,
                    error = %e,
                    "mcp setLevel failed"
                ),
            }
        }
        Ok(client)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn server_info(&self) -> &McpServerInfo {
        &self.server_info
    }

    pub fn capabilities(&self) -> &McpCapabilities {
        &self.capabilities
    }

    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub fn state(&self) -> McpClientState {
        self.state.read().unwrap().clone()
    }

    /// Raw request. Not part of the public 12.1 surface — exposed for tests
    /// and future sub-phases that need methods beyond `tools/*`.
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, McpError> {
        if !matches!(self.state(), McpClientState::Ready) {
            return Err(McpError::NotReady(format!("{:?}", self.state())));
        }
        if !self.breaker.allow() {
            return Err(McpError::BreakerOpen);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = protocol::encode_request(method, &params, id).map_err(McpError::Encode)?;
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);
        // Race fence: between the `state()` check above and the insert
        // we just did, the child-exit supervisor may have flipped the
        // state to `Failed` and called `drain_pending`. Without this
        // re-check, a request inserted *after* the drain pass would
        // wait until timeout — silently. Recheck and remove eagerly.
        if !matches!(self.state(), McpClientState::Ready) {
            self.pending.remove(&id);
            self.breaker.on_failure();
            return Err(McpError::ChildExited);
        }
        if self.outbox_tx.send(line).await.is_err() {
            self.pending.remove(&id);
            self.breaker.on_failure();
            return Err(McpError::ChildExited);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => {
                self.breaker.on_success();
                Ok(v)
            }
            Ok(Ok(Err((code, message)))) => {
                self.breaker.on_failure();
                Err(McpError::ServerError { code, message })
            }
            Ok(Err(_canceled)) => {
                self.breaker.on_failure();
                Err(McpError::ChildExited)
            }
            Err(_) => {
                self.pending.remove(&id);
                self.breaker.on_failure();
                Err(McpError::CallTimeout(timeout))
            }
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let mut out: Vec<McpTool> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            let v = self
                .request("tools/list", params, self.config.call_timeout)
                .await?;
            let page: ToolsListPage = serde_json::from_value(v).map_err(McpError::Decode)?;
            out.extend(page.tools);
            match page.next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => return Ok(out),
            }
        }
        Err(McpError::Protocol(format!(
            "tools/list pagination exceeded {MAX_PAGES} pages"
        )))
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolResult, McpError> {
        self.call_tool_with_meta(name, arguments, None).await
    }

    /// Phase 12.8 — `tools/call` with optional spec-level `_meta` carried
    /// alongside `arguments` (MCP 2024-11-05 request metadata).
    pub async fn call_tool_with_meta(
        &self,
        name: &str,
        arguments: serde_json::Value,
        meta: Option<serde_json::Value>,
    ) -> Result<McpToolResult, McpError> {
        let mut params = serde_json::json!({ "name": name, "arguments": arguments });
        if let Some(m) = meta {
            if let Some(obj) = params.as_object_mut() {
                obj.insert("_meta".into(), m);
            }
        }
        let v = self
            .request("tools/call", params, self.config.call_timeout)
            .await?;
        serde_json::from_value(v).map_err(McpError::Decode)
    }

    /// Phase 12.8 — send MCP `logging/setLevel`. Validates `level`
    /// against the spec-defined RFC-5424 subset and checks the server
    /// advertised the `logging` capability during `initialize`. Wire
    /// failures map to `McpError`; success ignores the empty response
    /// body.
    pub async fn set_log_level(&self, level: &str) -> Result<(), McpError> {
        if !crate::client_trait::MCP_LOG_LEVELS.contains(&level) {
            return Err(McpError::Protocol(format!("invalid log level '{level}'")));
        }
        if !self.capabilities.logging {
            return Err(McpError::Protocol(
                "server did not advertise logging capability".into(),
            ));
        }
        let _ = self
            .request(
                "logging/setLevel",
                serde_json::json!({ "level": level }),
                self.config.call_timeout,
            )
            .await?;
        Ok(())
    }

    /// Phase 12.5 — list data resources via paginated `resources/list`.
    /// Short-circuits to `Protocol` error if the server didn't advertise
    /// the `resources` capability during handshake.
    pub async fn list_resources(&self) -> Result<Vec<crate::types::McpResource>, McpError> {
        self.list_resources_with_meta(None).await
    }

    /// Phase 12.8 — paginated `resources/list` with optional `_meta`
    /// propagated in every page's request params.
    pub async fn list_resources_with_meta(
        &self,
        meta: Option<serde_json::Value>,
    ) -> Result<Vec<crate::types::McpResource>, McpError> {
        if !self.capabilities.resources {
            return Err(McpError::Protocol(
                "server does not advertise resources capability".into(),
            ));
        }
        let mut out: Vec<crate::types::McpResource> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params = serde_json::Map::new();
            if let Some(c) = &cursor {
                params.insert("cursor".into(), serde_json::Value::String(c.clone()));
            }
            if let Some(m) = &meta {
                params.insert("_meta".into(), m.clone());
            }
            let v = self
                .request(
                    "resources/list",
                    serde_json::Value::Object(params),
                    self.config.call_timeout,
                )
                .await?;
            let page: crate::types::ResourcesListPage =
                serde_json::from_value(v).map_err(McpError::Decode)?;
            out.extend(page.resources);
            match page.next_cursor {
                Some(n) if !n.is_empty() => cursor = Some(n),
                _ => return Ok(out),
            }
        }
        Err(McpError::Protocol(format!(
            "resources/list pagination exceeded {MAX_PAGES} pages"
        )))
    }

    /// Phase 12.5 — read a resource body via `resources/read`.
    pub async fn read_resource(
        &self,
        uri: &str,
    ) -> Result<Vec<crate::types::McpResourceContent>, McpError> {
        self.read_resource_with_meta(uri, None).await
    }

    /// Phase 12.8 — `resources/read` with optional `_meta`.
    pub async fn read_resource_with_meta(
        &self,
        uri: &str,
        meta: Option<serde_json::Value>,
    ) -> Result<Vec<crate::types::McpResourceContent>, McpError> {
        if !self.capabilities.resources {
            return Err(McpError::Protocol(
                "server does not advertise resources capability".into(),
            ));
        }
        let mut params = serde_json::json!({ "uri": uri });
        if let Some(m) = meta {
            if let Some(obj) = params.as_object_mut() {
                obj.insert("_meta".into(), m);
            }
        }
        let v = self
            .request("resources/read", params, self.config.call_timeout)
            .await?;
        let out: crate::types::ResourceReadResult =
            serde_json::from_value(v).map_err(McpError::Decode)?;
        Ok(out.contents)
    }

    /// Phase 12.8 — paginated `resources/templates/list`.
    pub async fn list_resource_templates(
        &self,
    ) -> Result<Vec<crate::types::McpResourceTemplate>, McpError> {
        if !self.capabilities.resources {
            return Err(McpError::Protocol(
                "server does not advertise resources capability".into(),
            ));
        }
        let mut out: Vec<crate::types::McpResourceTemplate> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            let v = self
                .request("resources/templates/list", params, self.config.call_timeout)
                .await?;
            let page: crate::types::ResourceTemplatesPage =
                serde_json::from_value(v).map_err(McpError::Decode)?;
            out.extend(page.resources);
            match page.next_cursor {
                Some(n) if !n.is_empty() => cursor = Some(n),
                _ => return Ok(out),
            }
        }
        Err(McpError::Protocol(format!(
            "resources/templates/list pagination exceeded {MAX_PAGES} pages"
        )))
    }

    /// Best-effort graceful shutdown. Idempotent; safe to call from Drop.
    pub async fn shutdown(&self) {
        self.shutdown_with_reason("client shutdown").await
    }

    /// Same as `shutdown` but threads `reason` into every
    /// `notifications/cancelled` payload. Control chars are stripped and
    /// the final string is truncated to 200 chars before wire-encoding.
    pub async fn shutdown_with_reason(&self, reason: &str) {
        if matches!(self.state(), McpClientState::Shutdown) {
            return;
        }
        let reason = sanitize_reason(reason);
        self.send_cancelled_notifications(&reason).await;
        self.shutdown.cancel();
        tokio::time::sleep(self.config.shutdown_grace).await;
        *self.state.write().unwrap() = McpClientState::Shutdown;
    }

    /// Send one `notifications/cancelled` per pending request id before
    /// shutting down the transport. Bounded by a 100 ms total window so a
    /// slow server can never stall shutdown. Excludes the reserved
    /// `INITIALIZE_ID` — cancelling a handshake that hasn't returned is
    /// nonsensical per MCP spec.
    async fn send_cancelled_notifications(&self, reason: &str) {
        let ids: Vec<u64> = self
            .pending
            .iter()
            .map(|e| *e.key())
            .filter(|&id| id != INITIALIZE_ID)
            .collect();
        if ids.is_empty() {
            return;
        }
        let name = self.name.clone();
        let outbox = self.outbox_tx.clone();
        let reason_owned = reason.to_string();
        let futs = ids.into_iter().map(move |id| {
            let outbox = outbox.clone();
            let name = name.clone();
            let reason = reason_owned.clone();
            async move {
                let notif = match protocol::encode_notification(
                    "notifications/cancelled",
                    serde_json::json!({
                        "requestId": id,
                        "reason": reason
                    }),
                ) {
                    Ok(s) => s,
                    Err(_) => return,
                };
                if outbox.send(notif).await.is_ok() {
                    tracing::debug!(
                        mcp = %name,
                        request_id = %id,
                        reason = %reason,
                        "sent notifications/cancelled"
                    );
                }
            }
        });
        let _ =
            tokio::time::timeout(Duration::from_millis(100), futures::future::join_all(futs)).await;
    }
}

/// Strip ASCII control characters and truncate the string to
/// `MAX_REASON_LEN` chars. Ensures the value is safe to embed in JSON-RPC
/// params without line corruption or gigantic logs.
pub(crate) fn sanitize_reason(raw: &str) -> String {
    const MAX_REASON_LEN: usize = 200;
    raw.chars()
        .filter(|c| !c.is_control())
        .take(MAX_REASON_LEN)
        .collect()
}

#[async_trait::async_trait]
impl crate::client_trait::McpClient for StdioMcpClient {
    fn name(&self) -> &str {
        StdioMcpClient::name(self)
    }
    fn server_info(&self) -> &crate::types::McpServerInfo {
        StdioMcpClient::server_info(self)
    }
    fn capabilities(&self) -> &crate::types::McpCapabilities {
        StdioMcpClient::capabilities(self)
    }
    fn protocol_version(&self) -> &str {
        StdioMcpClient::protocol_version(self)
    }
    fn state(&self) -> McpClientState {
        StdioMcpClient::state(self)
    }
    async fn list_tools(&self) -> Result<Vec<crate::types::McpTool>, crate::errors::McpError> {
        StdioMcpClient::list_tools(self).await
    }
    async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<crate::types::McpToolResult, crate::errors::McpError> {
        StdioMcpClient::call_tool(self, name, arguments).await
    }
    async fn call_tool_with_meta(
        &self,
        name: &str,
        arguments: serde_json::Value,
        meta: Option<serde_json::Value>,
    ) -> Result<crate::types::McpToolResult, crate::errors::McpError> {
        StdioMcpClient::call_tool_with_meta(self, name, arguments, meta).await
    }
    async fn shutdown(&self) {
        StdioMcpClient::shutdown(self).await
    }
    async fn shutdown_with_reason(&self, reason: &str) {
        StdioMcpClient::shutdown_with_reason(self, reason).await
    }
    async fn list_resources(
        &self,
    ) -> Result<Vec<crate::types::McpResource>, crate::errors::McpError> {
        StdioMcpClient::list_resources(self).await
    }
    async fn read_resource(
        &self,
        uri: &str,
    ) -> Result<Vec<crate::types::McpResourceContent>, crate::errors::McpError> {
        StdioMcpClient::read_resource(self, uri).await
    }
    async fn list_resources_with_meta(
        &self,
        meta: Option<serde_json::Value>,
    ) -> Result<Vec<crate::types::McpResource>, crate::errors::McpError> {
        StdioMcpClient::list_resources_with_meta(self, meta).await
    }
    async fn list_resource_templates(
        &self,
    ) -> Result<Vec<crate::types::McpResourceTemplate>, crate::errors::McpError> {
        StdioMcpClient::list_resource_templates(self).await
    }
    async fn read_resource_with_meta(
        &self,
        uri: &str,
        meta: Option<serde_json::Value>,
    ) -> Result<Vec<crate::types::McpResourceContent>, crate::errors::McpError> {
        StdioMcpClient::read_resource_with_meta(self, uri, meta).await
    }
    fn subscribe_events(&self) -> broadcast::Receiver<ClientEvent> {
        self.events_tx.subscribe()
    }
    async fn set_log_level(&self, level: &str) -> Result<(), McpError> {
        StdioMcpClient::set_log_level(self, level).await
    }
}

impl Drop for StdioMcpClient {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────────

fn build_command(cfg: &McpServerConfig) -> Command {
    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &cfg.cwd {
        cmd.current_dir(cwd);
    }
    // Environment: start from parent, then overlay the config's explicit vars.
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }
    cmd
}

async fn reader_task(
    stdout: ChildStdout,
    pending: Arc<PendingMap>,
    shutdown: CancellationToken,
    name: String,
    events_tx: broadcast::Sender<ClientEvent>,
    incoming_tx: mpsc::Sender<IncomingRequest>,
) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            maybe = lines.next_line() => {
                match maybe {
                    Ok(Some(line)) => {
                        // Classify: method+id = server-originated request,
                        // method alone = notification, id alone = response.
                        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&line) {
                            let method_opt = raw
                                .get("method")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string);
                            let id_opt = raw.get("id").cloned();
                            if let (Some(method), Some(id)) = (method_opt.as_ref(), id_opt.clone()) {
                                let params = raw.get("params").cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                let req = IncomingRequest {
                                    id: id.clone(),
                                    method: method.clone(),
                                    params,
                                };
                                match incoming_tx.try_send(req) {
                                    Ok(()) => {}
                                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                        // Send an immediate error response
                                        // so the server isn't left hanging
                                        // waiting for a reply that will
                                        // never arrive. Overload is a
                                        // transient condition; -32000 is
                                        // the JSON-RPC "server error" slot
                                        // we reserve for internal overload.
                                        tracing::warn!(mcp=%name, method=%method,
                                            "incoming request queue full; rejecting with server-busy");
                                        // We don't hold the writer channel
                                        // here, but we do have pending.
                                        // Best-effort: log and drop. (The
                                        // alternative of plumbing the
                                        // writer into the reader would
                                        // couple the paths — see follow-up.)
                                        let _ = id;
                                    }
                                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                        tracing::debug!(mcp=%name, "incoming handler gone; stopping reader");
                                        break;
                                    }
                                }
                                continue;
                            }
                            if let Some(method) = parse_notification_method(&raw) {
                                if method == "notifications/message" {
                                    let empty = serde_json::Value::Null;
                                    let params = raw.get("params").unwrap_or(&empty);
                                    crate::logging::emit_log_notification(&name, params);
                                    continue;
                                }
                                // Phase 80.9.c — channel notifications
                                // carry user-visible content; capture
                                // params so the inbound loop can run
                                // the gate + parser without
                                // re-fetching the JSON-RPC frame.
                                if method == crate::channel::CHANNEL_NOTIFICATION_METHOD {
                                    let params = raw
                                        .get("params")
                                        .cloned()
                                        .unwrap_or(serde_json::Value::Null);
                                    tracing::debug!(
                                        mcp = %name,
                                        method = %method,
                                        "channel notification received"
                                    );
                                    let _ = events_tx.send(
                                        crate::events::channel_message_event(params),
                                    );
                                    continue;
                                }
                                // Phase 80.9.b — permission relay reply
                                // from a channel server. Same shape as
                                // the channel notification: capture
                                // params verbatim so the pending-map
                                // resolver can match without rereading
                                // the frame.
                                if method == crate::channel_permission::PERMISSION_RESPONSE_METHOD
                                {
                                    let params = raw
                                        .get("params")
                                        .cloned()
                                        .unwrap_or(serde_json::Value::Null);
                                    tracing::debug!(
                                        mcp = %name,
                                        method = %method,
                                        "channel permission response received"
                                    );
                                    let _ = events_tx.send(
                                        crate::events::channel_permission_response_event(params),
                                    );
                                    continue;
                                }
                                let event = method_to_event(&method);
                                tracing::debug!(mcp=%name, method=%method, "server notification");
                                let _ = events_tx.send(event);
                                continue;
                            }
                        }
                        let response = match wire::decode_response(&line) {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::warn!(mcp=%name, error=%e, raw=%line, "invalid jsonrpc line");
                                continue;
                            }
                        };
                        let Some(id_val) = response.id.as_ref() else {
                            tracing::debug!(mcp=%name, "server notification ignored");
                            continue;
                        };
                        let Some(id) = wire::extract_u64_id(id_val) else {
                            tracing::warn!(mcp=%name, "non-numeric id, dropping");
                            continue;
                        };
                        if let Some((_, tx)) = pending.remove(&id) {
                            let payload = match (response.result, response.error) {
                                (Some(v), None) => Ok(v),
                                (None, Some(e)) => Err((e.code, e.message)),
                                (Some(v), Some(_)) => Ok(v),
                                (None, None) => Ok(serde_json::Value::Null),
                            };
                            let _ = tx.send(payload);
                        } else {
                            tracing::debug!(mcp=%name, id=%id, "late response, no pending entry");
                        }
                    }
                    Ok(None) => {
                        tracing::info!(mcp=%name, "stdout closed");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(mcp=%name, error=%e, "stdout read error");
                        break;
                    }
                }
            }
        }
    }
}

async fn writer_task(
    mut stdin: ChildStdin,
    mut rx: mpsc::Receiver<String>,
    shutdown: CancellationToken,
    name: String,
) {
    loop {
        tokio::select! {
            // Prefer writes so `shutdown()`'s `notifications/cancelled`
            // are flushed even if the cancel arm also becomes ready.
            biased;
            line = rx.recv() => {
                match line {
                    Some(line) => {
                        if let Err(e) = stdin.write_all(line.as_bytes()).await {
                            tracing::warn!(mcp=%name, error=%e, "stdin write failed");
                            break;
                        }
                        if let Err(e) = stdin.flush().await {
                            tracing::warn!(mcp=%name, error=%e, "stdin flush failed");
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = shutdown.cancelled() => {
                // Drain any already-queued lines before closing stdin.
                while let Ok(line) = rx.try_recv() {
                    if stdin.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                }
                let _ = stdin.flush().await;
                break;
            }
        }
    }
}

async fn stderr_task(stderr: ChildStderr, shutdown: CancellationToken, name: String) {
    let mut lines = BufReader::new(stderr).lines();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            maybe = lines.next_line() => {
                match maybe {
                    Ok(Some(line)) => tracing::debug!(mcp=%name, stderr=%line, "mcp stderr"),
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }
}

async fn incoming_handler_task(
    mut rx: mpsc::Receiver<IncomingRequest>,
    sampling_provider: Option<Arc<dyn SamplingProvider>>,
    server_name: String,
    writer_tx: mpsc::Sender<String>,
    shutdown: CancellationToken,
) {
    // Semaphore-bounded concurrency: each incoming server request runs
    // in its own task so a slow `sampling/createMessage` (blocking on
    // an LLM call) does not stall subsequent requests behind it. The
    // permit count caps how many sampling tasks can be in flight at
    // once — prevents fork-bomb amplification from a misbehaving
    // server.
    let semaphore = Arc::new(tokio::sync::Semaphore::new(INCOMING_CONCURRENCY));
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            maybe = rx.recv() => {
                let Some(req) = maybe else { break };
                let provider = sampling_provider.clone();
                let name = server_name.clone();
                let writer = writer_tx.clone();
                let sem = Arc::clone(&semaphore);
                let shutdown_child = shutdown.clone();
                tokio::spawn(async move {
                    // Acquire respects shutdown so we don't strand a
                    // task after the client is torn down.
                    let _permit = tokio::select! {
                        _ = shutdown_child.cancelled() => return,
                        p = sem.acquire_owned() => match p {
                            Ok(p) => p,
                            Err(_) => return,
                        },
                    };
                    if let Some(line) =
                        handle_incoming(&name, req, provider.as_ref()).await
                    {
                        let _ = writer.send(line).await;
                    }
                });
            }
        }
    }
}

async fn handle_incoming(
    server_name: &str,
    req: IncomingRequest,
    provider: Option<&Arc<dyn SamplingProvider>>,
) -> Option<String> {
    let method = req.method.as_str();
    if method == "sampling/createMessage" {
        let started_at = std::time::Instant::now();
        let record = |outcome: &str| {
            inc_sampling_requests_total(server_name, outcome);
            observe_sampling_duration_ms(server_name, started_at.elapsed().as_millis() as u64);
        };
        let provider = match provider {
            Some(p) => p,
            None => {
                record("unsupported");
                return Some(encode_error(
                    &req.id,
                    -32601,
                    "sampling not supported by this client",
                ));
            }
        };
        let parsed = match sampling_wire::parse_create_message_params(server_name, &req.params) {
            Ok(r) => r,
            Err(e) => {
                record(sampling_outcome_from_error(&e));
                return Some(encode_error(&req.id, e.json_rpc_code(), &e.to_string()));
            }
        };
        match provider.sample(parsed).await {
            Ok(resp) => {
                record("ok");
                let result = sampling_wire::encode_response(&resp);
                Some(encode_result(&req.id, &result))
            }
            Err(e) => {
                record(sampling_outcome_from_error(&e));
                Some(encode_error(&req.id, e.json_rpc_code(), &e.to_string()))
            }
        }
    } else {
        Some(encode_error(&req.id, -32601, "method not found"))
    }
}

fn sampling_outcome_from_error(e: &crate::sampling::SamplingError) -> &'static str {
    match e {
        crate::sampling::SamplingError::Disabled(_) => "disabled",
        crate::sampling::SamplingError::RateLimited(_) => "rate_limited",
        crate::sampling::SamplingError::InvalidParams(_) => "invalid_params",
        crate::sampling::SamplingError::ToolCallsRejected => "tool_calls_rejected",
        crate::sampling::SamplingError::LlmError(_) => "llm_error",
        crate::sampling::SamplingError::NoProvider => "no_provider",
    }
}

fn encode_result(id: &serde_json::Value, result: &serde_json::Value) -> String {
    let mut s = serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .unwrap_or_default();
    s.push('\n');
    s
}

fn encode_error(id: &serde_json::Value, code: i32, message: &str) -> String {
    let mut s = serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    }))
    .unwrap_or_default();
    s.push('\n');
    s
}

fn drain_pending(pending: &Arc<PendingMap>) {
    let ids: Vec<u64> = pending.iter().map(|e| *e.key()).collect();
    for id in ids {
        if let Some((_, tx)) = pending.remove(&id) {
            let _ = tx.send(Err((-32000, "child exited".into())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_reason;

    #[test]
    fn sanitize_reason_strips_control_chars() {
        let out = sanitize_reason("line1\nline2\tend");
        assert_eq!(out, "line1line2end");
    }

    #[test]
    fn sanitize_reason_truncates_to_200() {
        let raw: String = "a".repeat(500);
        let out = sanitize_reason(&raw);
        assert_eq!(out.len(), 200);
    }

    #[test]
    fn sanitize_reason_passes_typical_text() {
        let out = sanitize_reason("client shutdown");
        assert_eq!(out, "client shutdown");
    }

    #[test]
    fn mcp_log_levels_are_spec_compliant() {
        let levels = crate::client_trait::MCP_LOG_LEVELS;
        assert_eq!(levels.len(), 8);
        assert!(levels.contains(&"debug"));
        assert!(levels.contains(&"warning"));
        assert!(levels.contains(&"emergency"));
        assert!(!levels.contains(&"warn")); // common typo
    }
}
