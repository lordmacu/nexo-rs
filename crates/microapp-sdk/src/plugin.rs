//! Phase 81.15.a — Child-side helper for out-of-tree subprocess
//! plugins. Pairs with `nexo-core`'s `SubprocessNexoPlugin`
//! host-side adapter (Phase 81.14 + 81.14.b).
//!
//! Plugin authors avoid hand-rolling the JSON-RPC parser, manifest
//! handshake, and broker-publish framing by using
//! [`PluginAdapter`]:
//!
//! ```no_run
//! # #[cfg(feature = "plugin")] {
//! use nexo_microapp_sdk::plugin::{PluginAdapter, BrokerSender};
//! use nexo_broker::Event;
//!
//! const MANIFEST: &str = include_str!("../nexo-plugin.toml");
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     PluginAdapter::new(MANIFEST)?
//!         .on_broker_event(|topic, event, broker: BrokerSender| async move {
//!             // Plugin's outbound logic — e.g. send to Slack API,
//!             // then publish a confirmation event back.
//!             let ack = Event::new(
//!                 format!("plugin.inbound.slack"),
//!                 "slack",
//!                 serde_json::json!({"echo": event.payload}),
//!             );
//!             let _ = broker.publish("plugin.inbound.slack", ack).await;
//!         })
//!         .on_shutdown(|| async { Ok(()) })
//!         .run_stdio()
//!         .await
//! }
//! # }
//! ```
//!
//! # Wire format
//!
//! Same JSON-RPC 2.0 newline-delimited shape as the host adapter:
//! - `initialize` request → `{ manifest, server_version }` reply
//! - `broker.event { topic, event }` notification → user handler
//! - User handler may call `BrokerSender::publish` → emits
//!   `broker.publish` notification on stdout
//! - `shutdown` request → `{ ok: true }` reply, then loop exits
//!
//! # IRROMPIBLE refs
//!
//! - Internal: `crates/microapp-sdk/src/runtime.rs:87-264` —
//!   existing JSON-RPC dispatch loop pattern reused structurally
//!   (different methods, but same line/parse/dispatch shape).
//! - Internal: `crates/core/src/agent/nexo_plugin_registry/subprocess.rs`
//!   — host-side wire spec the SDK must match.
//! - claude-code-leak `src/utils/computerUse/mcpServer.ts` — MCP
//!   child-side server pattern (initialize, notifications without
//!   `id`).
//! - OpenClaw absence: their channel plugins ran in-process Node,
//!   no separate child-side SDK.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use nexo_broker::Event;
use nexo_llm::types::ChatMessage;
use nexo_memory::MemoryEntry;
use nexo_plugin_manifest::PluginManifest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{
    self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::{oneshot, Mutex};

use crate::errors::{Error as SdkError, Result as SdkResult};

/// Boxed future returned by user-supplied handlers. Avoids forcing
/// downstream authors to import `futures` crate just to type a
/// closure.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Handler invoked when the daemon delivers a `broker.event`
/// notification. Receives the topic, the parsed `Event`, and a
/// [`BrokerSender`] handle so the handler can publish back to the
/// daemon without holding any global state.
///
/// Errors from the handler are intentionally swallowed — same
/// best-effort contract the host adapter uses for `broker.publish`
/// forwarding. A handler that wants to surface errors should log
/// + drop on its own.
pub trait BrokerEventHandler: Send + Sync + 'static {
    /// Process one event delivered from the daemon.
    fn handle(&self, topic: String, event: Event, broker: BrokerSender) -> BoxFuture<'static, ()>;
}

impl<F, Fut> BrokerEventHandler for F
where
    F: Fn(String, Event, BrokerSender) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn handle(&self, topic: String, event: Event, broker: BrokerSender) -> BoxFuture<'static, ()> {
        Box::pin((self)(topic, event, broker))
    }
}

/// Handler invoked when the daemon sends `shutdown`. Called BEFORE
/// the SDK writes the `{ok:true}` reply, so the handler can flush
/// state. Errors propagate as `PluginInitError::Other` on the host
/// side via the `shutdown` reply error path.
pub trait ShutdownHandler: Send + Sync + 'static {
    /// Hook called once at shutdown; return `Ok(())` for clean
    /// exit, `Err(_)` to surface a structured error.
    fn handle(&self) -> BoxFuture<'static, Result<(), String>>;
}

impl<F, Fut> ShutdownHandler for F
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    fn handle(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin((self)())
    }
}

/// Phase 81.15.c — child-side request-response correlation map.
/// Each outbound request (memory.recall, llm.complete, ...) is
/// keyed by an integer id; the dispatch loop's reader looks up
/// the matching pending oneshot and resolves it when the host
/// replies. Reserved ids: 1 = (host→child) initialize, 2 =
/// (host→child) shutdown — both flow the OPPOSITE direction so
/// they never collide with the child's outbound id space (which
/// starts at 100).
type ChildPending = Arc<DashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>;

/// Default timeout for child-issued RPC requests. The daemon's
/// `memory.recall` returns in milliseconds; `llm.complete` can
/// take seconds for large responses (especially without
/// streaming). 30 s is comfortably above worst-case for both.
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Child id allocator. Starts at 100 to leave headroom below
/// the host's reserved ids (1 / 2).
fn next_request_id(counter: &AtomicU64) -> u64 {
    counter.fetch_add(1, Ordering::Relaxed)
}

/// Phase 81.15.c — error returned by child-issued RPC requests.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// Host returned a JSON-RPC error response. `code` is the
    /// JSON-RPC error code; common values: -32601 method not
    /// found, -32602 invalid params, -32603 internal error /
    /// "memory not configured" / "llm not configured".
    #[error("rpc error {code}: {message}")]
    Server {
        /// JSON-RPC error code from the host.
        code: i32,
        /// Human-readable error message from the host.
        message: String,
    },
    /// No reply within `DEFAULT_RPC_TIMEOUT` (30 s). The pending
    /// entry is removed so a late reply is silently dropped
    /// (with a warn log on the dispatch loop side).
    #[error("rpc request timed out after {0:?}")]
    Timeout(Duration),
    /// stdin writer is closed (host crashed / shutdown raced) or
    /// the response oneshot was canceled before it was resolved.
    #[error("rpc transport closed before reply: {0}")]
    Transport(String),
    /// Response payload could not be deserialized into the typed
    /// wrapper's expected shape. Shouldn't fire in well-formed
    /// host implementations — flagged loud so SDK + host stay
    /// in sync.
    #[error("rpc decode error: {0}")]
    Decode(String),
}

/// Child-side handle for the daemon-mediated services pipeline.
///
/// **Notifications (publish-only):**
/// - `publish(topic, event)` — emits a `broker.publish`
///   notification. Host validates the topic against its allowlist.
///
/// **Requests (request-response, Phase 81.15.c):**
/// - `recall_memory(agent_id, query, limit)` —
///   long-term memory FTS recall.
/// - `complete_llm(params)` — LLM chat completion (non-streaming
///   today; streaming via `params.stream = true` ships in
///   Phase 81.15.c.b SDK).
///
/// Cheap to clone (`Arc` internals). Plugin authors typically
/// receive one inside their `BrokerEventHandler` and clone for
/// background tasks.
#[derive(Clone)]
pub struct BrokerSender {
    writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    pending: ChildPending,
    next_id: Arc<AtomicU64>,
}

/// Phase 81.15.c — typed params for `complete_llm`. Mirrors the
/// wire shape in `nexo-plugin-contract.md` §5.2.
#[derive(Debug, Clone, Default)]
pub struct LlmCompleteParams {
    /// Provider name as registered in the operator's `llm.yaml`
    /// (e.g. `"minimax"`, `"openai"`).
    pub provider: String,
    /// Model identifier handed to the provider client.
    pub model: String,
    /// Chat messages forming the prompt. Empty rejected
    /// host-side with `-32602`.
    pub messages: Vec<ChatMessage>,
    /// Optional max tokens cap. Defaults host-side to 4096.
    pub max_tokens: Option<u32>,
    /// Optional sampling temperature. Defaults host-side to 0.7.
    pub temperature: Option<f32>,
    /// Optional system prompt prepended to messages.
    pub system_prompt: Option<String>,
}

/// Phase 81.15.c — typed result from `complete_llm`. Mirrors the
/// host-side `handle_llm_complete` response shape. Local
/// `TokenCount` shape (instead of `nexo_llm::TokenUsage`) keeps
/// the SDK independent of any serde-derive quirks upstream.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LlmCompleteResult {
    /// Full assistant text. Empty when streaming is enabled (the
    /// child reassembled it from `llm.complete.delta`
    /// notifications) or when the provider returned tool calls
    /// (which the MVP rejects with -32601).
    #[serde(default)]
    pub content: String,
    /// One of: `stop`, `length`, `tool_use`, `other:<reason>`.
    pub finish_reason: String,
    /// Token usage counts the provider reported.
    pub usage: TokenCount,
}

/// Phase 81.15.c — token usage count returned in
/// `LlmCompleteResult.usage`. Same shape as the host emits.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TokenCount {
    /// Tokens consumed by the prompt (input).
    #[serde(default)]
    pub prompt_tokens: u32,
    /// Tokens consumed by the completion (output).
    #[serde(default)]
    pub completion_tokens: u32,
}

impl BrokerSender {
    /// Phase 81.15.c — issue an RPC request to the daemon and
    /// await the response. Allocates a fresh id, registers a
    /// oneshot in the pending map, writes the request frame,
    /// then awaits the response with a 30 s timeout. On timeout
    /// the pending entry is removed; a delayed reply is dropped
    /// silently with a debug log.
    ///
    /// Low-level helper. Plugin authors typically use the typed
    /// wrappers `recall_memory()` / `complete_llm()` instead.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, RpcError> {
        let id = next_request_id(&self.next_id);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (tx, rx) = oneshot::channel::<Result<Value, RpcError>>();
        self.pending.insert(id, tx);

        // Serialize + write atomically under the writer lock so a
        // concurrent publish() can't interleave bytes mid-frame.
        let line = serde_json::to_string(&frame).map_err(|e| {
            self.pending.remove(&id);
            RpcError::Decode(format!("serialize request: {e}"))
        })?;
        {
            let mut w = self.writer.lock().await;
            if w.write_all(line.as_bytes()).await.is_err()
                || w.write_all(b"\n").await.is_err()
                || w.flush().await.is_err()
            {
                self.pending.remove(&id);
                return Err(RpcError::Transport(
                    "stdin write failed (host closed?)".to_string(),
                ));
            }
        }

        let timeout = timeout.unwrap_or(DEFAULT_RPC_TIMEOUT);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(payload)) => payload,
            Ok(Err(_canceled)) => {
                // Pending oneshot canceled before reply — host
                // dispatch loop most likely exited mid-request.
                self.pending.remove(&id);
                Err(RpcError::Transport(
                    "response oneshot canceled before reply".to_string(),
                ))
            }
            Err(_elapsed) => {
                // Timeout. Remove the pending so a late reply is
                // dropped instead of leaking memory.
                self.pending.remove(&id);
                Err(RpcError::Timeout(timeout))
            }
        }
    }

    /// Phase 81.15.c — typed wrapper for `memory.recall`. Asks
    /// the daemon's long-term memory for entries matching `query`
    /// for `agent_id`, capped at `limit` results. Returns the
    /// deserialized `Vec<MemoryEntry>` from the response payload.
    ///
    /// Errors:
    /// - [`RpcError::Server`] with `-32603` when the operator
    ///   hasn't configured long-term memory.
    /// - [`RpcError::Server`] with `-32602` for bad params.
    /// - [`RpcError::Timeout`] after 30 s default.
    pub async fn recall_memory(
        &self,
        agent_id: &str,
        query: &str,
        limit: u64,
    ) -> Result<Vec<MemoryEntry>, RpcError> {
        let params = json!({
            "agent_id": agent_id,
            "query": query,
            "limit": limit,
        });
        let result = self.request("memory.recall", params, None).await?;
        let entries_val = result.get("entries").cloned().unwrap_or(Value::Null);
        serde_json::from_value::<Vec<MemoryEntry>>(entries_val)
            .map_err(|e| RpcError::Decode(format!("memory.recall entries: {e}")))
    }

    /// Phase 81.15.c — typed wrapper for `llm.complete`
    /// (non-streaming). Builds the JSON-RPC params from
    /// [`LlmCompleteParams`], issues the request, deserializes
    /// the response into [`LlmCompleteResult`].
    ///
    /// Streaming consumption ships in 81.15.c.b — that variant
    /// will return an `impl Stream<Item = String>` of text
    /// chunks plus a final `LlmCompleteResult` (without
    /// `content`).
    ///
    /// Errors mirror the host wire spec at
    /// `nexo-plugin-contract.md` §5.2.
    pub async fn complete_llm(
        &self,
        p: LlmCompleteParams,
    ) -> Result<LlmCompleteResult, RpcError> {
        let mut params = json!({
            "provider": p.provider,
            "model": p.model,
            "messages": p.messages,
        });
        if let Some(max) = p.max_tokens {
            params["max_tokens"] = json!(max);
        }
        if let Some(temp) = p.temperature {
            params["temperature"] = json!(temp);
        }
        if let Some(sys) = p.system_prompt {
            params["system_prompt"] = json!(sys);
        }
        let result = self.request("llm.complete", params, None).await?;
        serde_json::from_value::<LlmCompleteResult>(result)
            .map_err(|e| RpcError::Decode(format!("llm.complete result: {e}")))
    }

    /// Emit `broker.publish { topic, event }` on stdout. The host
    /// validates the topic against its allowlist before forwarding
    /// to the broker — bad publishes get dropped (with a warn-level
    /// log on the host side).
    pub async fn publish(&self, topic: &str, event: Event) -> SdkResult<()> {
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "broker.publish",
            "params": { "topic": topic, "event": event },
        });
        let line = serde_json::to_string(&frame).map_err(|e| {
            SdkError::Io(io::Error::new(io::ErrorKind::Other, e.to_string()))
        })?;
        let mut writer = self.writer.lock().await;
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }
}

/// Builder for the child-side plugin runtime. Authors call
/// [`PluginAdapter::new`] with their manifest TOML, register
/// `on_broker_event` + `on_shutdown` handlers, then drive the
/// dispatch loop with `run_stdio`.
pub struct PluginAdapter {
    cached_manifest: PluginManifest,
    server_version: String,
    on_broker_event: Option<Arc<dyn BrokerEventHandler>>,
    on_shutdown: Option<Arc<dyn ShutdownHandler>>,
}

impl PluginAdapter {
    /// Parse the bundled manifest. Plugin authors typically pass
    /// the result of `include_str!("../nexo-plugin.toml")`. The
    /// manifest's `plugin.id` becomes the identity the daemon
    /// validates after `initialize`.
    pub fn new(manifest_toml: &str) -> SdkResult<Self> {
        let cached_manifest: PluginManifest = toml::from_str(manifest_toml).map_err(|e| {
            SdkError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("PluginAdapter: parse manifest TOML failed: {e}"),
            ))
        })?;
        let server_version = format!(
            "{}-{}",
            cached_manifest.plugin.id, cached_manifest.plugin.version
        );
        Ok(Self {
            cached_manifest,
            server_version,
            on_broker_event: None,
            on_shutdown: None,
        })
    }

    /// Override the default `server_version` (which defaults to
    /// `<plugin.id>-<plugin.version>` from the manifest). Useful
    /// when the binary's runtime version differs from the manifest
    /// version (e.g. a hot-patched build).
    pub fn with_server_version(mut self, version: impl Into<String>) -> Self {
        self.server_version = version.into();
        self
    }

    /// Register the handler invoked for each `broker.event`
    /// notification the daemon delivers. Without one, events are
    /// silently dropped.
    pub fn on_broker_event<H: BrokerEventHandler>(mut self, handler: H) -> Self {
        self.on_broker_event = Some(Arc::new(handler));
        self
    }

    /// Register the handler invoked when the daemon sends
    /// `shutdown`. Called BEFORE the reply, so the handler can
    /// flush state; an `Err` propagates as JSON-RPC error so the
    /// host surfaces `PluginShutdownError::Other`.
    pub fn on_shutdown<H: ShutdownHandler>(mut self, handler: H) -> Self {
        self.on_shutdown = Some(Arc::new(handler));
        self
    }

    /// Drive the dispatch loop on stdin/stdout until the daemon
    /// sends `shutdown` or stdin reaches EOF.
    pub async fn run_stdio(self) -> SdkResult<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        self.run(BufReader::new(stdin), stdout).await
    }

    /// Drive the dispatch loop on caller-supplied IO. Used by unit
    /// tests via `tokio::io::duplex` and by integration tests that
    /// want to inject mocks.
    pub async fn run<R, W>(self, reader: R, writer: W) -> SdkResult<()>
    where
        R: AsyncBufRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>> =
            Arc::new(Mutex::new(Box::new(writer)));
        dispatch_loop(reader, writer, self).await
    }
}

/// Inner dispatch loop. Reads JSON-RPC lines, demuxes by method,
/// invokes user handlers, writes replies. Returns on EOF or
/// `shutdown`.
async fn dispatch_loop<R>(
    reader: R,
    writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    adapter: PluginAdapter,
) -> SdkResult<()>
where
    R: AsyncBufRead + Unpin + Send + 'static,
{
    let mut lines = reader.lines();
    let manifest_value = serde_json::to_value(&adapter.cached_manifest).map_err(|e| {
        SdkError::Io(io::Error::new(io::ErrorKind::Other, e.to_string()))
    })?;
    // Phase 81.15.c — child-side request-response correlation.
    // Each outbound request (memory.recall / llm.complete / ...)
    // registers a oneshot here under its allocated id; the reader
    // demuxes response frames (id + result/error, no method) back
    // to the matching pending entry.
    let pending: ChildPending = Arc::new(DashMap::new());
    let next_id: Arc<AtomicU64> = Arc::new(AtomicU64::new(100));
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                write_error(&writer, None, -32700, &format!("parse error: {e}")).await?;
                continue;
            }
        };
        let id = frame.get("id").cloned();
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let params = frame.get("params").cloned().unwrap_or(Value::Null);

        // Phase 81.15.c — RESPONSE to one of OUR outbound
        // requests: frame has `id` AND no `method` AND has
        // `result` or `error`. Look up in pending map; resolve
        // the oneshot. Out-of-order responses (id we don't
        // recognize) are dropped with a debug log — most likely
        // a delayed reply after timeout.
        if let Some(id_val) = id.as_ref() {
            if method.is_empty() {
                if let Some(req_id) = id_val.as_u64() {
                    if let Some((_, sender)) = pending.remove(&req_id) {
                        let payload = if let Some(err) = frame.get("error") {
                            let code = err
                                .get("code")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(-32603) as i32;
                            let message = err
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(no message)")
                                .to_string();
                            Err(RpcError::Server { code, message })
                        } else {
                            Ok(frame.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(payload);
                        continue;
                    }
                    tracing::debug!(
                        id = req_id,
                        "rpc response with unknown id — drop (likely after timeout)"
                    );
                    continue;
                }
            }
        }

        // Notifications carry no `id`. Today the only one we
        // accept is `broker.event`; everything else is dropped
        // with a debug log.
        if id.is_none() {
            if method == "broker.event" {
                let topic = params
                    .get("topic")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let event_val = params.get("event").cloned().unwrap_or(Value::Null);
                let event: Event = match serde_json::from_value(event_val) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, topic, "broker.event: deserialize Event failed — drop");
                        continue;
                    }
                };
                if let Some(handler) = &adapter.on_broker_event {
                    let sender = BrokerSender {
                        writer: writer.clone(),
                        pending: pending.clone(),
                        next_id: next_id.clone(),
                    };
                    // Phase 81.15.c — spawn the handler so the
                    // dispatch loop keeps reading the next line
                    // while the handler awaits any RPC responses.
                    // Without spawn, a handler that calls
                    // `broker.request(...)` deadlocks: the
                    // request's oneshot can only be resolved by
                    // the dispatch loop reading the response
                    // frame, but the loop is blocked awaiting
                    // the handler future itself.
                    let handler_clone = handler.clone();
                    tokio::spawn(async move {
                        handler_clone.handle(topic, event, sender).await;
                    });
                }
            } else {
                tracing::debug!(method, "unhandled notification — drop");
            }
            continue;
        }

        match method {
            "initialize" => {
                let result = json!({
                    "manifest": manifest_value,
                    "server_version": adapter.server_version,
                });
                write_result(&writer, id, result).await?;
            }
            "shutdown" => {
                if let Some(handler) = &adapter.on_shutdown {
                    match handler.handle().await {
                        Ok(()) => {
                            write_result(&writer, id, json!({"ok": true})).await?;
                        }
                        Err(e) => {
                            write_error(&writer, id, -32000, &e).await?;
                        }
                    }
                } else {
                    write_result(&writer, id, json!({"ok": true})).await?;
                }
                break;
            }
            other => {
                write_error(
                    &writer,
                    id,
                    -32601,
                    &format!("method not found: {other}"),
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn write_result(
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    id: Option<Value>,
    result: Value,
) -> SdkResult<()> {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result,
    });
    write_line(writer, &frame).await
}

async fn write_error(
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    id: Option<Value>,
    code: i32,
    message: &str,
) -> SdkResult<()> {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": code, "message": message },
    });
    write_line(writer, &frame).await
}

async fn write_line(
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    frame: &Value,
) -> SdkResult<()> {
    let line = serde_json::to_string(frame)
        .map_err(|e| SdkError::Io(io::Error::new(io::ErrorKind::Other, e.to_string())))?;
    let mut w = writer.lock().await;
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use tokio::io::{duplex, BufReader as TokioBufReader};

    const TEST_MANIFEST: &str = r#"
[plugin]
id = "test_plugin"
version = "0.1.0"
name = "test"
description = "fixture"
min_nexo_version = ">=0.1.0"
"#;

    /// Spawn the adapter on a duplex pipe + return helpers to
    /// drive it from the test's side: write requests, read
    /// replies. The adapter task is moved off so the test can
    /// proceed with assertions.
    async fn run_adapter_on_duplex(
        adapter: PluginAdapter,
    ) -> (
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
        TokioBufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        tokio::task::JoinHandle<SdkResult<()>>,
    ) {
        // Two duplex pipes: host_to_plugin (test writes, adapter
        // reads) + plugin_to_host (adapter writes, test reads).
        let (host_writer_end, plugin_reader_end) = duplex(8192);
        let (plugin_writer_end, host_reader_end) = duplex(8192);
        let plugin_reader = TokioBufReader::new(plugin_reader_end);
        let plugin_writer = plugin_writer_end;
        let join = tokio::spawn(adapter.run(plugin_reader, plugin_writer));
        let (_unused_read, host_write) = tokio::io::split(host_writer_end);
        let (host_read, _unused_write) = tokio::io::split(host_reader_end);
        (host_write, TokioBufReader::new(host_read), join)
    }

    async fn read_reply_line(
        reader: &mut TokioBufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    ) -> Value {
        let mut buf = String::new();
        reader.read_line(&mut buf).await.expect("read reply line");
        serde_json::from_str(buf.trim()).expect("reply parses as JSON")
    }

    #[tokio::test]
    async fn initialize_replies_with_cached_manifest() {
        let adapter = PluginAdapter::new(TEST_MANIFEST).expect("manifest parses");
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["manifest"]["plugin"]["id"], "test_plugin");
        assert_eq!(reply["result"]["server_version"], "test_plugin-0.1.0");
    }

    #[tokio::test]
    async fn broker_event_dispatches_to_user_handler() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(
                move |topic: String, event: Event, _broker: BrokerSender| {
                    let called = called_clone.clone();
                    async move {
                        assert_eq!(topic, "plugin.outbound.test");
                        assert_eq!(event.source, "host");
                        called.store(true, Ordering::SeqCst);
                    }
                },
            );
        let (mut host_write, _host_read, _join) = run_adapter_on_duplex(adapter).await;
        // Fabricate a broker.event notification.
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "broker.event",
            "params": {
                "topic": "plugin.outbound.test",
                "event": {
                    "id": "00000000-0000-0000-0000-000000000010",
                    "timestamp": "2026-05-01T00:00:00Z",
                    "topic": "plugin.outbound.test",
                    "source": "host",
                    "session_id": null,
                    "payload": {"hello": "world"},
                }
            }
        });
        let line = format!("{}\n", serde_json::to_string(&frame).unwrap());
        host_write.write_all(line.as_bytes()).await.unwrap();
        // Give the adapter a tick to process; tighter than 100ms
        // makes flaky.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(called.load(Ordering::SeqCst), "handler must be invoked");
    }

    #[tokio::test]
    async fn broker_sender_writes_publish_notification() {
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(
                |_topic: String, event: Event, broker: BrokerSender| async move {
                    let echo = Event::new(
                        "plugin.inbound.test",
                        "plugin",
                        serde_json::json!({"echo": event.payload}),
                    );
                    broker
                        .publish("plugin.inbound.test", echo)
                        .await
                        .expect("publish ok");
                },
            );
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "broker.event",
            "params": {
                "topic": "plugin.outbound.test",
                "event": {
                    "id": "00000000-0000-0000-0000-000000000010",
                    "timestamp": "2026-05-01T00:00:00Z",
                    "topic": "plugin.outbound.test",
                    "source": "host",
                    "session_id": null,
                    "payload": {"foo": "bar"},
                }
            }
        });
        let line = format!("{}\n", serde_json::to_string(&frame).unwrap());
        host_write.write_all(line.as_bytes()).await.unwrap();
        // Read the next outbound line — must be a broker.publish
        // notification carrying the echo payload.
        let reply = read_reply_line(&mut host_read).await;
        assert!(reply.get("id").is_none(), "publish must have NO id");
        assert_eq!(reply["method"], "broker.publish");
        assert_eq!(reply["params"]["topic"], "plugin.inbound.test");
        assert_eq!(reply["params"]["event"]["payload"]["echo"]["foo"], "bar");
    }

    #[tokio::test]
    async fn shutdown_invokes_handler_and_breaks_loop() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_clone = calls.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_shutdown(move || {
                let calls = calls_clone.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            });
        let (mut host_write, mut host_read, join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"shutdown\",\"params\":{}}\n")
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["ok"], true);
        // Loop must exit after shutdown.
        let res = tokio::time::timeout(std::time::Duration::from_millis(500), join).await;
        assert!(
            res.is_ok(),
            "dispatch loop must exit promptly after shutdown"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_method_returns_neg_32601() {
        let adapter = PluginAdapter::new(TEST_MANIFEST).expect("manifest parses");
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"bogus\",\"params\":{}}\n")
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["id"], 5);
        assert_eq!(reply["error"]["code"], -32601);
        assert!(reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bogus"));
    }

    #[tokio::test]
    async fn parse_error_returns_neg_32700() {
        let adapter = PluginAdapter::new(TEST_MANIFEST).expect("manifest parses");
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write.write_all(b"not-json\n").await.unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["error"]["code"], -32700);
    }

    /// Phase 81.15.c — `BrokerSender::request` issues a JSON-RPC
    /// request with an allocated id, then awaits the response on
    /// the dispatch loop's pending map. We drive the adapter from
    /// inside a `broker.event` handler that calls `request()`,
    /// then the test side reads the outgoing request frame from
    /// the adapter's stdout, sends a synthetic response back via
    /// the adapter's stdin, and asserts the handler observed the
    /// expected result. Round-trip end-to-end.
    #[tokio::test]
    async fn request_helper_round_trips_via_dispatch_loop() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let observed = Arc::new(AtomicBool::new(false));
        let observed_clone = observed.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(
                move |_topic, _event, broker: BrokerSender| {
                    let observed = observed_clone.clone();
                    async move {
                        let result = broker
                            .request(
                                "test.echo",
                                serde_json::json!({"x": 1}),
                                Some(Duration::from_secs(1)),
                            )
                            .await;
                        match result {
                            Ok(v) => {
                                assert_eq!(v["echoed"], 1);
                                observed.store(true, Ordering::SeqCst);
                            }
                            Err(e) => {
                                panic!("request must succeed, got {e}")
                            }
                        }
                    }
                },
            );
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;

        // Trigger the handler: send a broker.event notification.
        let trigger = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "broker.event",
            "params": {
                "topic": "plugin.outbound.test",
                "event": {
                    "id": "00000000-0000-0000-0000-000000000010",
                    "timestamp": "2026-05-01T00:00:00Z",
                    "topic": "plugin.outbound.test",
                    "source": "host",
                    "session_id": null,
                    "payload": {}
                }
            }
        });
        host_write
            .write_all(format!("{}\n", trigger).as_bytes())
            .await
            .unwrap();

        // Read the outgoing request frame the handler issued.
        let request_frame = read_reply_line(&mut host_read).await;
        assert_eq!(request_frame["method"], "test.echo");
        let req_id = request_frame["id"].as_u64().expect("id is u64");
        assert!(req_id >= 100, "child ids start at 100, got {req_id}");
        assert_eq!(request_frame["params"]["x"], 1);

        // Send the response back. Match the id; carry an `echoed`
        // value the handler will assert against.
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {"echoed": request_frame["params"]["x"]},
        });
        host_write
            .write_all(format!("{}\n", response).as_bytes())
            .await
            .unwrap();

        // Wait briefly for the handler to observe + assert.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            observed.load(Ordering::SeqCst),
            "handler must observe the response"
        );
    }

    /// Phase 81.15.c — when the host returns a JSON-RPC error
    /// response, `request()` propagates as `RpcError::Server`
    /// with the code + message preserved.
    #[tokio::test]
    async fn request_helper_propagates_server_error() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let observed = Arc::new(AtomicBool::new(false));
        let observed_clone = observed.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(move |_t, _e, broker: BrokerSender| {
                let observed = observed_clone.clone();
                async move {
                    let result = broker
                        .request(
                            "memory.recall",
                            serde_json::json!({"agent_id": "x", "query": "x"}),
                            Some(Duration::from_secs(1)),
                        )
                        .await;
                    match result {
                        Err(RpcError::Server { code, message }) => {
                            assert_eq!(code, -32603);
                            assert!(message.contains("not configured"));
                            observed.store(true, Ordering::SeqCst);
                        }
                        other => panic!("expected RpcError::Server, got {other:?}"),
                    }
                }
            });
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;

        // Trigger.
        let trigger = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "broker.event",
            "params": {
                "topic": "plugin.outbound.test",
                "event": {
                    "id": "00000000-0000-0000-0000-000000000010",
                    "timestamp": "2026-05-01T00:00:00Z",
                    "topic": "plugin.outbound.test",
                    "source": "host",
                    "session_id": null,
                    "payload": {}
                }
            }
        });
        host_write
            .write_all(format!("{}\n", trigger).as_bytes())
            .await
            .unwrap();

        let req = read_reply_line(&mut host_read).await;
        let err_resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": req["id"],
            "error": { "code": -32603, "message": "memory not configured" }
        });
        host_write
            .write_all(format!("{}\n", err_resp).as_bytes())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            observed.load(Ordering::SeqCst),
            "handler must observe RpcError::Server"
        );
    }

    /// Phase 81.15.c — when no response arrives within the
    /// timeout, `request()` returns `RpcError::Timeout` and
    /// the pending entry is removed (so a delayed reply is
    /// dropped silently rather than leaking memory).
    #[tokio::test]
    async fn request_helper_times_out_when_host_silent() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let observed = Arc::new(AtomicBool::new(false));
        let observed_clone = observed.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(move |_t, _e, broker: BrokerSender| {
                let observed = observed_clone.clone();
                async move {
                    let result = broker
                        .request(
                            "test.silent",
                            serde_json::json!({}),
                            Some(Duration::from_millis(150)),
                        )
                        .await;
                    match result {
                        Err(RpcError::Timeout(_)) => {
                            observed.store(true, Ordering::SeqCst);
                        }
                        other => panic!("expected Timeout, got {other:?}"),
                    }
                }
            });
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;

        let trigger = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "broker.event",
            "params": {
                "topic": "plugin.outbound.test",
                "event": {
                    "id": "00000000-0000-0000-0000-000000000010",
                    "timestamp": "2026-05-01T00:00:00Z",
                    "topic": "plugin.outbound.test",
                    "source": "host",
                    "session_id": null,
                    "payload": {}
                }
            }
        });
        host_write
            .write_all(format!("{}\n", trigger).as_bytes())
            .await
            .unwrap();

        // Drain the outgoing request frame so it doesn't pile up;
        // never send a response.
        let _req = read_reply_line(&mut host_read).await;

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            observed.load(Ordering::SeqCst),
            "handler must observe Timeout"
        );
    }

    /// Phase 81.15.c — `recall_memory()` typed wrapper deserializes
    /// the `entries` array from the response into `Vec<MemoryEntry>`.
    /// Bad shape surfaces as `RpcError::Decode`.
    #[tokio::test]
    async fn recall_memory_typed_wrapper_round_trips() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let observed = Arc::new(AtomicBool::new(false));
        let observed_clone = observed.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(move |_t, _e, broker: BrokerSender| {
                let observed = observed_clone.clone();
                async move {
                    let result = broker.recall_memory("agent_x", "preference", 5).await;
                    match result {
                        Ok(entries) => {
                            assert_eq!(entries.len(), 1);
                            assert_eq!(entries[0].agent_id, "agent_x");
                            observed.store(true, Ordering::SeqCst);
                        }
                        Err(e) => panic!("recall_memory must succeed, got {e}"),
                    }
                }
            });
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;

        let trigger = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "broker.event",
            "params": {
                "topic": "plugin.outbound.test",
                "event": {
                    "id": "00000000-0000-0000-0000-000000000010",
                    "timestamp": "2026-05-01T00:00:00Z",
                    "topic": "plugin.outbound.test",
                    "source": "host",
                    "session_id": null,
                    "payload": {}
                }
            }
        });
        host_write
            .write_all(format!("{}\n", trigger).as_bytes())
            .await
            .unwrap();

        // Adapter issues the memory.recall request; respond with
        // a fabricated entries array shaped like nexo_memory::MemoryEntry.
        let req = read_reply_line(&mut host_read).await;
        assert_eq!(req["method"], "memory.recall");
        assert_eq!(req["params"]["agent_id"], "agent_x");
        assert_eq!(req["params"]["query"], "preference");
        assert_eq!(req["params"]["limit"], 5);
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {
                "entries": [{
                    "id": "00000000-0000-0000-0000-000000000001",
                    "agent_id": "agent_x",
                    "content": "user prefers concise",
                    "tags": ["preference"],
                    "concept_tags": [],
                    "created_at": "2026-05-01T00:00:00Z",
                    "memory_type": null
                }]
            }
        });
        host_write
            .write_all(format!("{}\n", response).as_bytes())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            observed.load(Ordering::SeqCst),
            "recall_memory wrapper must deserialize entries"
        );
    }
}
