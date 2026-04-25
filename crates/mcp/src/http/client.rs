//! HTTP MCP client — both `streamable-http` (modern) and `sse` (legacy).
//!
//! Skeleton defined in step 6; the handshake/request methods land in the
//! subsequent steps of Phase 12.2.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use dashmap::DashMap;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::{broadcast, oneshot, RwLock as TokioRwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

use agent_extensions::runtime::wire;
use agent_resilience::{CircuitBreaker, CircuitBreakerConfig};

use crate::client::McpClientState;
use crate::client_trait::McpClient;
use crate::errors::McpError;
use crate::events::{method_to_event, ClientEvent};
use crate::http::redact::redact_sensitive_url;
use crate::http::sse::{run_sse_reader, SseEvent};
use crate::protocol::{
    self, parse_notification_method, InitializeParams, INITIALIZE_ID, PROTOCOL_VERSION,
};
use crate::types::{
    InitializeResult, McpCapabilities, McpClientInfo, McpServerInfo, McpTool, McpToolResult,
    ToolsListPage,
};

const MAX_PAGES: usize = 64;
const SESSION_HEADER: &str = "mcp-session-id";
const EVENT_CAP: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpTransportMode {
    StreamableHttp,
    Sse,
    /// Phase 12.2 follow-up — try `StreamableHttp` first; if the initial
    /// POST returns 404/405/415 (server doesn't speak streamable), fall
    /// back to `Sse`. The client's `transport()` accessor reports the
    /// resolved mode after `connect`, not `Auto`.
    Auto,
}

#[derive(Debug, Clone)]
pub struct HttpMcpOptions {
    pub connect_timeout: Duration,
    pub initialize_timeout: Duration,
    pub call_timeout: Duration,
    pub shutdown_grace: Duration,
    pub keepalive_timeout: Duration,
    pub sse_reconnect_base: Duration,
    pub sse_reconnect_max: Duration,
    pub sse_reconnect_max_attempts: u32,
    /// Phase 12.8 — if Some and server advertises `logging` capability,
    /// client sends `logging/setLevel` once post-handshake.
    pub log_level: Option<String>,
}

impl Default for HttpMcpOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            initialize_timeout: Duration::from_secs(10),
            call_timeout: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(3),
            keepalive_timeout: Duration::from_secs(60),
            sse_reconnect_base: Duration::from_secs(1),
            sse_reconnect_max: Duration::from_secs(30),
            sse_reconnect_max_attempts: 10,
            log_level: None,
        }
    }
}

type PendingMap = DashMap<u64, oneshot::Sender<Result<serde_json::Value, (i32, String)>>>;

pub struct HttpMcpClient {
    name: String,
    url: Url,
    transport: HttpTransportMode,
    headers: HeaderMap,
    http: reqwest::Client,
    server_info: McpServerInfo,
    capabilities: McpCapabilities,
    protocol_version: String,
    state: Arc<RwLock<McpClientState>>,
    session_id: Arc<TokioRwLock<Option<String>>>,
    breaker: Arc<CircuitBreaker>,
    next_id: Arc<AtomicU64>,
    opts: HttpMcpOptions,
    pending: Arc<PendingMap>,
    post_url: Arc<TokioRwLock<Option<Url>>>, // SSE only
    shutdown: CancellationToken,
    sse_handle: std::sync::Mutex<Option<JoinHandle<()>>>,
    events_tx: broadcast::Sender<ClientEvent>,
    /// Ids for which a request has been sent but no response seen yet.
    /// Unlike `pending` (SSE only), this tracks inflight across both
    /// transports so `shutdown()` can emit `notifications/cancelled` for
    /// them. For SSE this is redundant with `pending` but harmless.
    inflight: Arc<DashMap<u64, ()>>,
}

impl HttpMcpClient {
    pub async fn connect(
        name: String,
        url: Url,
        transport: HttpTransportMode,
        headers: HeaderMap,
        opts: HttpMcpOptions,
    ) -> Result<Self, McpError> {
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(McpError::Protocol(format!(
                "only http/https urls supported, got {}",
                url.scheme()
            )));
        }
        let breaker = Arc::new(CircuitBreaker::new(
            format!("mcp:{name}"),
            CircuitBreakerConfig::default(),
        ));
        // Persist cookies across calls on the same session — a handful
        // of MCP servers (notably custom Streamable HTTP backends with
        // CDN-style routing) expect the client to echo `Set-Cookie`.
        // Standard spec traffic doesn't need this, but enabling it is a
        // free win when it matters.
        let http = reqwest::Client::builder()
            .connect_timeout(opts.connect_timeout)
            .cookie_store(true)
            .build()
            .map_err(|e| McpError::Http(e.to_string()))?;

        let state = Arc::new(RwLock::new(McpClientState::Connecting));
        let session_id = Arc::new(TokioRwLock::new(None));
        let pending: Arc<PendingMap> = Arc::new(DashMap::new());
        let post_url = Arc::new(TokioRwLock::new(None));
        let shutdown = CancellationToken::new();
        let (events_tx, _) = broadcast::channel::<ClientEvent>(EVENT_CAP);

        let (init, sse_handle, resolved_transport) = match transport {
            HttpTransportMode::StreamableHttp => {
                let init =
                    handshake_streamable(&http, &url, &headers, &session_id, &opts, &name).await?;
                (init, None, HttpTransportMode::StreamableHttp)
            }
            HttpTransportMode::Sse => {
                let (init, handle) = handshake_sse(
                    &http,
                    &url,
                    &headers,
                    &pending,
                    &post_url,
                    &opts,
                    &shutdown,
                    &name,
                    events_tx.clone(),
                )
                .await?;
                (init, Some(handle), HttpTransportMode::Sse)
            }
            HttpTransportMode::Auto => {
                match handshake_streamable(&http, &url, &headers, &session_id, &opts, &name).await {
                    Ok(init) => (init, None, HttpTransportMode::StreamableHttp),
                    Err(e) if is_streamable_unsupported(&e) => {
                        tracing::info!(
                            mcp = %name,
                            error = %e,
                            "streamable_http rejected; falling back to sse"
                        );
                        // Fresh session slot for the sse attempt.
                        *session_id.write().await = None;
                        let (init, handle) = handshake_sse(
                            &http,
                            &url,
                            &headers,
                            &pending,
                            &post_url,
                            &opts,
                            &shutdown,
                            &name,
                            events_tx.clone(),
                        )
                        .await?;
                        (init, Some(handle), HttpTransportMode::Sse)
                    }
                    Err(e) => return Err(e),
                }
            }
        };

        *state.write().unwrap() = McpClientState::Ready;

        let client = Self {
            name,
            url,
            transport: resolved_transport,
            headers,
            http,
            server_info: init.server_info,
            capabilities: init.capabilities,
            protocol_version: if init.protocol_version.is_empty() {
                PROTOCOL_VERSION.to_string()
            } else {
                init.protocol_version
            },
            state,
            session_id,
            breaker,
            next_id: Arc::new(AtomicU64::new(1)),
            opts,
            pending,
            post_url,
            shutdown,
            sse_handle: std::sync::Mutex::new(sse_handle),
            events_tx,
            inflight: Arc::new(DashMap::new()),
        };
        if let Some(level) = client.opts.log_level.clone() {
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
    pub fn url(&self) -> &Url {
        &self.url
    }
    pub fn transport(&self) -> HttpTransportMode {
        self.transport
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

    // ─── request paths ─────────────────────────────────────────────────────

    /// Send a JSON-RPC message via the active transport and return the
    /// correlated response value. Handles breaker and timeouts.
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
        let body = line.trim_end().to_string();
        self.inflight.insert(id, ());

        let outcome = match self.transport {
            HttpTransportMode::Auto => {
                unreachable!("auto resolves to streamable/sse during connect")
            }
            HttpTransportMode::StreamableHttp => {
                match tokio::time::timeout(
                    timeout,
                    post_streamable_raw(
                        &self.http,
                        &self.url,
                        &self.headers,
                        &self.session_id,
                        &self.name,
                        body,
                    ),
                )
                .await
                {
                    Ok(Ok(response)) => {
                        if let StreamableResponse::Stream(ref messages) = response {
                            emit_notifications(messages, &self.events_tx, &self.name);
                        }
                        extract_response_by_id(response, id)
                    }
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(McpError::CallTimeout(timeout)),
                }
            }
            HttpTransportMode::Sse => {
                let post_url = {
                    let guard = self.post_url.read().await;
                    guard
                        .clone()
                        .ok_or_else(|| McpError::Protocol("sse not connected".into()))?
                };
                let (tx, rx) = oneshot::channel();
                self.pending.insert(id, tx);
                let post_outcome = post_sse_raw(&self.http, &post_url, &self.headers, body).await;
                // If the POST itself errored, the request may still have
                // reached the server (in-flight packet). Don't drop the
                // pending slot eagerly — let the timeout absorb any
                // orphan response that lands afterwards. Surface the
                // post error only after the slot's lifetime is bounded.
                if let Err(e) = post_outcome {
                    let outcome = tokio::time::timeout(timeout, rx).await;
                    self.pending.remove(&id);
                    return match outcome {
                        Ok(Ok(Ok(v))) => Ok(v),
                        // Server eventually replied — caller wins.
                        _ => Err(e),
                    };
                }
                match tokio::time::timeout(timeout, rx).await {
                    Ok(Ok(Ok(v))) => Ok(v),
                    Ok(Ok(Err((code, message)))) => Err(McpError::ServerError { code, message }),
                    Ok(Err(_)) => Err(McpError::TransportLost("sse channel canceled".into())),
                    Err(_) => {
                        self.pending.remove(&id);
                        Err(McpError::CallTimeout(timeout))
                    }
                }
            }
        };

        self.inflight.remove(&id);
        match outcome {
            Ok(v) => {
                self.breaker.on_success();
                Ok(v)
            }
            Err(e) => {
                self.breaker.on_failure();
                Err(e)
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
                .request("tools/list", params, self.opts.call_timeout)
                .await?;
            let page: ToolsListPage = serde_json::from_value(v).map_err(McpError::Decode)?;
            out.extend(page.tools);
            match page.next_cursor {
                Some(n) if !n.is_empty() => cursor = Some(n),
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

    /// Phase 12.8 — `tools/call` with optional spec-level `_meta`
    /// alongside `arguments` (MCP 2024-11-05).
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
            .request("tools/call", params, self.opts.call_timeout)
            .await?;
        serde_json::from_value(v).map_err(McpError::Decode)
    }

    /// Phase 12.8 — `logging/setLevel`. Mirror of `StdioMcpClient`.
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
                self.opts.call_timeout,
            )
            .await?;
        Ok(())
    }

    pub async fn list_resources(&self) -> Result<Vec<crate::types::McpResource>, McpError> {
        self.list_resources_with_meta(None).await
    }

    /// Phase 12.8 — `resources/list` with optional `_meta`.
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
                    self.opts.call_timeout,
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
            .request("resources/read", params, self.opts.call_timeout)
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
                .request("resources/templates/list", params, self.opts.call_timeout)
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

    pub async fn shutdown(&self) {
        self.shutdown_with_reason("client shutdown").await
    }

    pub async fn shutdown_with_reason(&self, reason: &str) {
        if matches!(self.state(), McpClientState::Shutdown) {
            return;
        }
        let reason = crate::client::sanitize_reason(reason);
        // Best-effort: tell the server about in-flight calls before we
        // tear down the transport so it can release resources early.
        self.send_cancelled_notifications(&reason).await;
        self.shutdown.cancel();
        if let Some(h) = self.sse_handle.lock().unwrap().take() {
            h.abort();
        }
        // Drop all pending with TransportLost so callers unblock.
        let ids: Vec<u64> = self.pending.iter().map(|e| *e.key()).collect();
        for id in ids {
            if let Some((_, tx)) = self.pending.remove(&id) {
                let _ = tx.send(Err((-32099, "client shutdown".into())));
            }
        }
        tokio::time::sleep(self.opts.shutdown_grace.min(Duration::from_millis(200))).await;
        *self.state.write().unwrap() = McpClientState::Shutdown;
    }

    /// Fire-and-forget `notifications/cancelled` POST per pending id.
    /// Bounded by a 100 ms total window so a slow or dead server can't
    /// block shutdown.
    async fn send_cancelled_notifications(&self, reason: &str) {
        let ids: Vec<u64> = self
            .inflight
            .iter()
            .map(|e| *e.key())
            .filter(|&id| id != INITIALIZE_ID)
            .collect();
        if ids.is_empty() {
            return;
        }
        let reason_owned = reason.to_string();
        let post_url_sse = match self.transport {
            HttpTransportMode::Sse => self.post_url.read().await.clone(),
            _ => None,
        };
        let name = self.name.clone();
        let transport = self.transport;
        let futs = ids.into_iter().map(|id| {
            let http = self.http.clone();
            let url = self.url.clone();
            let headers = self.headers.clone();
            let session_id = self.session_id.clone();
            let post_url_sse = post_url_sse.clone();
            let name = name.clone();
            let reason = reason_owned.clone();
            async move {
                let body = match protocol::encode_notification(
                    "notifications/cancelled",
                    serde_json::json!({
                        "requestId": id,
                        "reason": reason
                    }),
                ) {
                    Ok(s) => s.trim_end().to_string(),
                    Err(_) => return,
                };
                let outcome = match transport {
                    HttpTransportMode::Auto => unreachable!("auto resolved during connect"),
                    HttpTransportMode::StreamableHttp => {
                        post_streamable_raw(&http, &url, &headers, &session_id, &name, body)
                            .await
                            .map(|_| ())
                    }
                    HttpTransportMode::Sse => match post_url_sse {
                        Some(u) => post_sse_raw(&http, &u, &headers, body).await,
                        None => return,
                    },
                };
                match outcome {
                    Ok(_) => tracing::debug!(
                        mcp = %name,
                        request_id = %id,
                        reason = %reason,
                        "sent notifications/cancelled"
                    ),
                    Err(e) => tracing::debug!(
                        mcp = %name,
                        request_id = %id,
                        reason = %reason,
                        error = %e,
                        "notifications/cancelled post failed"
                    ),
                }
            }
        });
        let _ =
            tokio::time::timeout(Duration::from_millis(100), futures::future::join_all(futs)).await;
    }
}

impl Drop for HttpMcpClient {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[async_trait::async_trait]
impl McpClient for HttpMcpClient {
    fn name(&self) -> &str {
        HttpMcpClient::name(self)
    }
    fn server_info(&self) -> &McpServerInfo {
        HttpMcpClient::server_info(self)
    }
    fn capabilities(&self) -> &McpCapabilities {
        HttpMcpClient::capabilities(self)
    }
    fn protocol_version(&self) -> &str {
        HttpMcpClient::protocol_version(self)
    }
    fn state(&self) -> McpClientState {
        HttpMcpClient::state(self)
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        HttpMcpClient::list_tools(self).await
    }
    async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolResult, McpError> {
        HttpMcpClient::call_tool(self, name, arguments).await
    }
    async fn call_tool_with_meta(
        &self,
        name: &str,
        arguments: serde_json::Value,
        meta: Option<serde_json::Value>,
    ) -> Result<McpToolResult, McpError> {
        HttpMcpClient::call_tool_with_meta(self, name, arguments, meta).await
    }
    async fn shutdown(&self) {
        HttpMcpClient::shutdown(self).await
    }
    async fn shutdown_with_reason(&self, reason: &str) {
        HttpMcpClient::shutdown_with_reason(self, reason).await
    }
    async fn list_resources(&self) -> Result<Vec<crate::types::McpResource>, McpError> {
        HttpMcpClient::list_resources(self).await
    }
    async fn read_resource(
        &self,
        uri: &str,
    ) -> Result<Vec<crate::types::McpResourceContent>, McpError> {
        HttpMcpClient::read_resource(self, uri).await
    }
    async fn list_resources_with_meta(
        &self,
        meta: Option<serde_json::Value>,
    ) -> Result<Vec<crate::types::McpResource>, McpError> {
        HttpMcpClient::list_resources_with_meta(self, meta).await
    }
    async fn read_resource_with_meta(
        &self,
        uri: &str,
        meta: Option<serde_json::Value>,
    ) -> Result<Vec<crate::types::McpResourceContent>, McpError> {
        HttpMcpClient::read_resource_with_meta(self, uri, meta).await
    }
    async fn list_resource_templates(
        &self,
    ) -> Result<Vec<crate::types::McpResourceTemplate>, McpError> {
        HttpMcpClient::list_resource_templates(self).await
    }
    fn subscribe_events(&self) -> broadcast::Receiver<ClientEvent> {
        self.events_tx.subscribe()
    }
    async fn set_log_level(&self, level: &str) -> Result<(), McpError> {
        HttpMcpClient::set_log_level(self, level).await
    }
}

// ─── internal types / helpers ─────────────────────────────────────────────

enum StreamableResponse {
    Single(serde_json::Value),
    Stream(Vec<serde_json::Value>),
    Empty,
}

/// Phase 12.2 follow-up — classify `Auto` mode fallback signals. Servers
/// that only speak SSE typically respond to a streamable POST with 404
/// (unknown route), 405 (method not allowed), or 415 (unsupported media
/// type — most SSE-only servers reject `application/json` POST).
pub(crate) fn is_streamable_unsupported(e: &McpError) -> bool {
    matches!(e, McpError::HttpStatus { status, .. } if matches!(*status, 404 | 405 | 415))
}

fn extract_jsonrpc(value: serde_json::Value, _id: u64) -> Result<serde_json::Value, McpError> {
    if let Some(err) = value.get("error") {
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) as i32;
        let message = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("server error")
            .to_string();
        return Err(McpError::ServerError { code, message });
    }
    match value.get("result").cloned() {
        Some(r) => Ok(r),
        None => Ok(serde_json::Value::Null),
    }
}

async fn handshake_streamable(
    http: &reqwest::Client,
    url: &Url,
    headers: &HeaderMap,
    session_id: &Arc<TokioRwLock<Option<String>>>,
    opts: &HttpMcpOptions,
    name: &str,
) -> Result<InitializeResult, McpError> {
    let client_info = McpClientInfo {
        name: "agent".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let params = InitializeParams::new(&client_info);
    let body =
        protocol::encode_request("initialize", &params, INITIALIZE_ID).map_err(McpError::Encode)?;

    let response = tokio::time::timeout(
        opts.initialize_timeout,
        post_streamable_raw(
            http,
            url,
            headers,
            session_id,
            name,
            body.trim_end().to_string(),
        ),
    )
    .await
    .map_err(|_| McpError::InitializeTimeout(opts.initialize_timeout))??;

    let value = extract_response_by_id(response, INITIALIZE_ID)?;
    let init: InitializeResult = serde_json::from_value(value).map_err(McpError::Decode)?;

    let notif = protocol::encode_notification("notifications/initialized", serde_json::json!({}))
        .map_err(McpError::Encode)?;
    let _ = post_streamable_raw(
        http,
        url,
        headers,
        session_id,
        name,
        notif.trim_end().to_string(),
    )
    .await;
    Ok(init)
}

#[allow(clippy::too_many_arguments)]
async fn handshake_sse(
    http: &reqwest::Client,
    url: &Url,
    headers: &HeaderMap,
    pending: &Arc<PendingMap>,
    post_url: &Arc<TokioRwLock<Option<Url>>>,
    opts: &HttpMcpOptions,
    shutdown: &CancellationToken,
    name: &str,
    events_tx: broadcast::Sender<ClientEvent>,
) -> Result<(InitializeResult, JoinHandle<()>), McpError> {
    let req = http.get(url.clone()).headers(headers.clone());
    let response = tokio::time::timeout(opts.initialize_timeout, req.send())
        .await
        .map_err(|_| McpError::InitializeTimeout(opts.initialize_timeout))?
        .map_err(|e| McpError::Http(redact_sensitive_url(&e.to_string())))?;
    if !response.status().is_success() {
        return Err(McpError::HttpStatus {
            status: response.status().as_u16(),
            body: response.text().await.unwrap_or_default(),
        });
    }

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<SseEvent>(64);
    let byte_stream = response.bytes_stream();
    let sh = shutdown.clone();
    let log_name = name.to_string();
    let reader = tokio::spawn(run_sse_reader(byte_stream, event_tx, sh, log_name));

    let endpoint = tokio::time::timeout(opts.initialize_timeout, event_rx.recv())
        .await
        .map_err(|_| McpError::InitializeTimeout(opts.initialize_timeout))?
        .ok_or_else(|| McpError::Protocol("sse stream closed before endpoint".into()))?;
    let post_url_raw = match endpoint {
        SseEvent::Endpoint(u) => u,
        SseEvent::Message(_) => {
            return Err(McpError::Protocol(
                "sse: first event must be `endpoint`".into(),
            ))
        }
    };
    let resolved_post_url = url
        .join(&post_url_raw)
        .map_err(|e| McpError::Protocol(format!("sse endpoint url: {e}")))?;
    *post_url.write().await = Some(resolved_post_url.clone());

    let dispatcher = {
        let pending = pending.clone();
        let name_cloned = name.to_string();
        let events_tx_cloned = events_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = event_rx.recv().await {
                if let SseEvent::Message(msg) = ev {
                    dispatch_sse_message(&pending, msg, &name_cloned, &events_tx_cloned);
                }
            }
        })
    };

    let (tx, rx) = oneshot::channel();
    pending.insert(INITIALIZE_ID, tx);
    let client_info = McpClientInfo {
        name: "agent".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let params = InitializeParams::new(&client_info);
    let body =
        protocol::encode_request("initialize", &params, INITIALIZE_ID).map_err(McpError::Encode)?;
    post_sse_raw(
        http,
        &resolved_post_url,
        headers,
        body.trim_end().to_string(),
    )
    .await?;
    let value = tokio::time::timeout(opts.initialize_timeout, rx)
        .await
        .map_err(|_| McpError::InitializeTimeout(opts.initialize_timeout))?
        .map_err(|_| McpError::Protocol("initialize canceled".into()))?
        .map_err(|(code, message)| McpError::ServerError { code, message })?;
    let init: InitializeResult = serde_json::from_value(value).map_err(McpError::Decode)?;

    let notif = protocol::encode_notification("notifications/initialized", serde_json::json!({}))
        .map_err(McpError::Encode)?;
    let _ = post_sse_raw(
        http,
        &resolved_post_url,
        headers,
        notif.trim_end().to_string(),
    )
    .await;

    let composite = tokio::spawn(async move {
        let _ = reader.await;
        dispatcher.abort();
    });

    Ok((init, composite))
}

/// Phase 12.2 follow-up — spec-compliant wrapper: when the server
/// returns 404 on a request that carried a live `Mcp-Session-Id`, the
/// session was invalidated server-side. Drop it and retry once without
/// the header so the next response reseeds a fresh session id.
async fn post_streamable_raw(
    http: &reqwest::Client,
    url: &Url,
    base_headers: &HeaderMap,
    session_id: &Arc<TokioRwLock<Option<String>>>,
    log_name: &str,
    body: String,
) -> Result<StreamableResponse, McpError> {
    let had_session = session_id.read().await.is_some();
    let outcome =
        post_streamable_once(http, url, base_headers, session_id, log_name, body.clone()).await;
    match outcome {
        Err(McpError::HttpStatus { status: 404, .. }) if had_session => {
            tracing::warn!(
                mcp = %log_name,
                "Mcp-Session-Id invalidated by server (404); retrying without session"
            );
            *session_id.write().await = None;
            post_streamable_once(http, url, base_headers, session_id, log_name, body).await
        }
        other => other,
    }
}

async fn post_streamable_once(
    http: &reqwest::Client,
    url: &Url,
    base_headers: &HeaderMap,
    session_id: &Arc<TokioRwLock<Option<String>>>,
    log_name: &str,
    body: String,
) -> Result<StreamableResponse, McpError> {
    let mut headers = base_headers.clone();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    if let Some(sid) = session_id.read().await.clone() {
        if let Ok(v) = HeaderValue::from_str(&sid) {
            if let Ok(name) = HeaderName::from_bytes(SESSION_HEADER.as_bytes()) {
                headers.insert(name, v);
            }
        }
    }

    let response = http
        .post(url.clone())
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|e| McpError::Http(redact_sensitive_url(&e.to_string())))?;

    if let Some(v) = response.headers().get(SESSION_HEADER) {
        if let Ok(s) = v.to_str() {
            *session_id.write().await = Some(s.to_string());
        }
    }
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(McpError::HttpStatus {
            status: status.as_u16(),
            body,
        });
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if content_type.starts_with("text/event-stream") {
        let stream = response.bytes_stream();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let shutdown = CancellationToken::new();
        let name = log_name.to_string();
        let reader = tokio::spawn(run_sse_reader(stream, tx, shutdown, name));
        let mut messages = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let SseEvent::Message(v) = ev {
                messages.push(v);
            }
        }
        let _ = reader.await;
        Ok(StreamableResponse::Stream(messages))
    } else {
        let body = response
            .text()
            .await
            .map_err(|e| McpError::Http(e.to_string()))?;
        let trimmed = body.trim();
        if trimmed.is_empty() {
            return Ok(StreamableResponse::Empty);
        }
        let value: serde_json::Value = serde_json::from_str(trimmed).map_err(McpError::Decode)?;
        Ok(StreamableResponse::Single(value))
    }
}

async fn post_sse_raw(
    http: &reqwest::Client,
    post_url: &Url,
    base_headers: &HeaderMap,
    body: String,
) -> Result<(), McpError> {
    let mut headers = base_headers.clone();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let response = http
        .post(post_url.clone())
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|e| McpError::Http(redact_sensitive_url(&e.to_string())))?;
    if !response.status().is_success() {
        return Err(McpError::HttpStatus {
            status: response.status().as_u16(),
            body: response.text().await.unwrap_or_default(),
        });
    }
    Ok(())
}

fn extract_response_by_id(
    response: StreamableResponse,
    id: u64,
) -> Result<serde_json::Value, McpError> {
    match response {
        StreamableResponse::Single(v) => extract_jsonrpc(v, id),
        StreamableResponse::Stream(messages) => {
            for msg in messages {
                if msg.get("id").and_then(|i| i.as_u64()) == Some(id) {
                    return extract_jsonrpc(msg, id);
                }
            }
            Err(McpError::Protocol(format!(
                "no response with id={id} in streamed body"
            )))
        }
        StreamableResponse::Empty => Err(McpError::Protocol("empty response body".into())),
    }
}

fn emit_notifications(
    messages: &[serde_json::Value],
    events_tx: &broadcast::Sender<ClientEvent>,
    log_name: &str,
) {
    for msg in messages {
        if let Some(method) = parse_notification_method(msg) {
            if method == "notifications/message" {
                let empty = serde_json::Value::Null;
                let params = msg.get("params").unwrap_or(&empty);
                crate::logging::emit_log_notification(log_name, params);
                continue;
            }
            tracing::debug!(mcp=%log_name, method=%method, "streamable notification");
            let _ = events_tx.send(method_to_event(&method));
        }
    }
}

fn dispatch_sse_message(
    pending: &Arc<PendingMap>,
    message: serde_json::Value,
    log_name: &str,
    events_tx: &broadcast::Sender<ClientEvent>,
) {
    // Notifications have no `id` — forward to event bus and return.
    if let Some(method) = parse_notification_method(&message) {
        if method == "notifications/message" {
            let empty = serde_json::Value::Null;
            let params = message.get("params").unwrap_or(&empty);
            crate::logging::emit_log_notification(log_name, params);
            return;
        }
        tracing::debug!(mcp=%log_name, method=%method, "sse notification");
        let _ = events_tx.send(method_to_event(&method));
        return;
    }
    let Some(id_val) = message.get("id") else {
        tracing::debug!(mcp=%log_name, "sse message without id or method dropped");
        return;
    };
    let Some(id) = wire::extract_u64_id(id_val) else {
        tracing::warn!(mcp=%log_name, "sse non-numeric id, dropping");
        return;
    };
    if let Some((_, tx)) = pending.remove(&id) {
        let payload = match (message.get("result").cloned(), message.get("error")) {
            (Some(r), _) => Ok(r),
            (None, Some(e)) => {
                let code = e.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) as i32;
                let message = e
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("server error")
                    .to_string();
                Err((code, message))
            }
            (None, None) => Ok(serde_json::Value::Null),
        };
        let _ = tx.send(payload);
    } else {
        tracing::debug!(mcp=%log_name, id=%id, "sse late response");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_is_sane() {
        let o = HttpMcpOptions::default();
        assert_eq!(o.connect_timeout, Duration::from_secs(30));
        assert_eq!(o.sse_reconnect_max_attempts, 10);
    }

    #[tokio::test]
    async fn rejects_non_http_url() {
        let url = Url::parse("ftp://example.com/").unwrap();
        match HttpMcpClient::connect(
            "x".into(),
            url,
            HttpTransportMode::StreamableHttp,
            HeaderMap::new(),
            HttpMcpOptions::default(),
        )
        .await
        {
            Ok(_) => panic!("expected Protocol error"),
            Err(McpError::Protocol(msg)) => assert!(msg.contains("only http/https")),
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
}
