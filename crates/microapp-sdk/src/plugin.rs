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
//!     Ok(PluginAdapter::new(MANIFEST)?
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
//!         .await?)
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
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

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
/// the matching pending entry and resolves it when the host
/// replies. Reserved ids: 1 = (host→child) initialize, 2 =
/// (host→child) shutdown — both flow the OPPOSITE direction so
/// they never collide with the child's outbound id space (which
/// starts at 100).
///
/// 81.15.c.b — pending value type changed from a single oneshot
/// to an enum [`PendingKind`] so streaming requests
/// (`complete_llm_stream`) can register both a `mpsc` receiver
/// for delta chunks AND a final oneshot for the
/// `LlmCompleteResult` reply.
type ChildPending = Arc<DashMap<u64, PendingKind>>;

/// Phase 81.15.c.b — variant of pending entry kept alive while a
/// child request is in flight. The dispatch loop's response
/// path resolves `Single` / `Streaming.final_tx`; the
/// notification path pushes chunks into `Streaming.delta_tx`.
#[doc(hidden)]
pub enum PendingKind {
    /// Non-streaming request. The dispatch loop resolves this
    /// oneshot once the response frame lands.
    Single(oneshot::Sender<Result<Value, RpcError>>),
    /// Streaming request. The dispatch loop pushes
    /// `llm.complete.delta` chunks into `delta_tx` as they
    /// arrive; the final response frame resolves `final_tx`
    /// (which then closes the stream from the user's side).
    Streaming {
        /// Per-request channel for delta chunks. Unbounded so a
        /// fast provider doesn't backpressure the dispatch loop;
        /// the buffer is reclaimed when the consumer drops the
        /// `LlmStream`.
        delta_tx: mpsc::UnboundedSender<String>,
        /// Resolved when the host's final response frame lands.
        final_tx: oneshot::Sender<Result<LlmCompleteResult, RpcError>>,
    },
}

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
        self.pending.insert(id, PendingKind::Single(tx));

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

    /// Phase 81.15.c.b — streaming variant of `complete_llm`.
    /// Issues the request with `stream: true` and returns an
    /// [`LlmStream`] handle the caller drives via
    /// [`LlmStream::next_chunk`] (delta chunks as they arrive)
    /// and [`LlmStream::await_final`] (final usage + finish
    /// reason after the stream closes). Dropping the
    /// `LlmStream` before the host sends its final response is
    /// safe — the pending entry is cleaned up via `Drop` so a
    /// late delta or final reply is silently discarded with a
    /// debug log.
    ///
    /// Errors:
    /// - [`RpcError::Transport`] when the stdin write fails
    ///   before the request leaves (host already closed).
    /// - The returned `LlmStream`'s `await_final()` resolves
    ///   with [`RpcError::Server`] when the host returns a
    ///   JSON-RPC error response (e.g. `-32603 "llm not
    ///   configured"`).
    pub async fn complete_llm_stream(&self, p: LlmCompleteParams) -> Result<LlmStream, RpcError> {
        let mut params = json!({
            "provider": p.provider,
            "model": p.model,
            "messages": p.messages,
            "stream": true,
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
        let id = next_request_id(&self.next_id);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "llm.complete",
            "params": params,
        });
        let (delta_tx, delta_rx) = mpsc::unbounded_channel::<String>();
        let (final_tx, final_rx) = oneshot::channel::<Result<LlmCompleteResult, RpcError>>();
        self.pending
            .insert(id, PendingKind::Streaming { delta_tx, final_tx });

        let line = serde_json::to_string(&frame).map_err(|e| {
            self.pending.remove(&id);
            RpcError::Decode(format!("serialize stream request: {e}"))
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
        Ok(LlmStream {
            request_id: id,
            chunks: delta_rx,
            finished: Some(final_rx),
            pending: self.pending.clone(),
        })
    }

    /// Phase 81.15.c — typed wrapper for `llm.complete`
    /// (non-streaming). Builds the JSON-RPC params from
    /// [`LlmCompleteParams`], issues the request, deserializes
    /// the response into [`LlmCompleteResult`].
    ///
    /// For streaming consumption use
    /// [`Self::complete_llm_stream`] instead — that variant
    /// returns an [`LlmStream`] handle yielding delta chunks +
    /// a final `LlmCompleteResult`.
    ///
    /// Errors mirror the host wire spec at
    /// `nexo-plugin-contract.md` §5.2.
    pub async fn complete_llm(&self, p: LlmCompleteParams) -> Result<LlmCompleteResult, RpcError> {
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
        let line = serde_json::to_string(&frame)
            .map_err(|e| SdkError::Io(io::Error::new(io::ErrorKind::Other, e.to_string())))?;
        let mut writer = self.writer.lock().await;
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }
}

// ────────────────────────────────────────────────────────────────
// Phase 81.17.c / 81.29.b — child-side tool dispatch
//
// Wire shape (contract v1.10.0 §5.t):
//   host  → child   `tool.invoke { plugin_id, tool_name, args, agent_id }`
//   child → host    `{ result }` or `{ error: { code, message } }`
//                   error band: -33401 NotFound .. -33405 Denied.
//
// Authors register tool defs declaratively via
// [`PluginAdapter::declare_tools`] (advertised in the initialize
// reply so the host's `RemoteToolHandler` registration succeeds —
// see `crates/core/src/agent/nexo_plugin_registry/subprocess.rs`)
// and a single dispatch closure via [`PluginAdapter::on_tool`].
// ────────────────────────────────────────────────────────────────

/// Declarative tool descriptor advertised in the `initialize` reply.
///
/// Wire-compatible with the host's
/// `nexo_core::agent::tool_remote::RemoteToolDef` — same field
/// names + `serde(rename_all)` so the JSON shape round-trips
/// without per-side translators.
///
/// `name` MUST appear in the manifest's `[plugin.extends] tools = [...]`
/// allowlist; advertising a name not in the manifest causes the
/// host to kill the subprocess at handshake (defense against
/// out-of-tree binaries advertising tools the operator did not
/// authorise).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolDef {
    /// LLM-facing tool name. Per 81.3 namespace policy must match
    /// `<plugin_id>_*` or `ext_<plugin_id>_*`.
    pub name: String,
    /// One-sentence description shown to the LLM in the tool
    /// catalogue. Keep concise; LLMs prune noisy descriptions.
    pub description: String,
    /// JSON Schema (object) for tool arguments. Must validate the
    /// payload the LLM produces; the host runs schema validation
    /// before round-tripping `tool.invoke` to the child.
    pub input_schema: serde_json::Value,
}

/// Decoded `tool.invoke` request as the host hands it to the
/// child-side handler. Field names mirror contract v1.10.0 §5.t.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ToolInvocation {
    /// Stable plugin id from the manifest (echoed by the host so
    /// multi-plugin handlers can dispatch).
    pub plugin_id: String,
    /// Canonical tool name — handlers route on this.
    pub tool_name: String,
    /// Tool-specific arguments. Defaults to `Value::Null` when the
    /// host omits the field.
    #[serde(default)]
    pub args: serde_json::Value,
    /// Agent id producing the call. `None` when the host
    /// dispatcher is operator-driven (admin RPC, debug CLI).
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// Failure modes the child can surface from a `tool.invoke`
/// handler. Each variant maps onto the `-33401..-33405` JSON-RPC
/// error band the host's `RemoteToolHandler` decodes (see
/// `nexo_core::agent::tool_remote::parse_tool_error_string`).
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ToolInvocationError {
    /// `-33401` — tool name not advertised by this plugin.
    /// Surfaces when the host dispatched a tool the manifest
    /// allows but the runtime handler doesn't recognise.
    #[error("tool not found: {0}")]
    NotFound(String),
    /// `-33402` — args failed handler-side validation. The host
    /// already ran JSON-Schema validation; this branch covers
    /// semantic checks the schema can't express.
    #[error("invalid argument: {0}")]
    ArgumentInvalid(String),
    /// `-33403` — handler ran but failed (network blip, browser
    /// crash, downstream API 5xx). LLM sees a soft failure and
    /// can route around.
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
    /// `-33404` — tool exists but cannot run right now (e.g.,
    /// missing binary on disk, dependency offline).
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// `-33405` — tool exists but the caller is not authorised.
    /// Reserved for future capability-aware ACLs (81.28.b).
    #[error("denied: {0}")]
    Denied(String),
}

impl ToolInvocationError {
    /// JSON-RPC error code corresponding to the variant. Matches
    /// the host's decoder; see contract v1.10.0 §5.t.
    pub fn code(&self) -> i32 {
        match self {
            Self::NotFound(_) => -33401,
            Self::ArgumentInvalid(_) => -33402,
            Self::ExecutionFailed(_) => -33403,
            Self::Unavailable(_) => -33404,
            Self::Denied(_) => -33405,
        }
    }
}

/// Async handler invoked by the dispatch loop on every
/// `tool.invoke` request the host sends. Plugin authors register
/// one via [`PluginAdapter::on_tool`]; the closure typically
/// matches on `inv.tool_name` and routes to per-tool logic.
///
/// Blanket-implemented for any `Fn(ToolInvocation) -> Fut` where
/// `Fut: Future<Output = Result<Value, ToolInvocationError>> + Send`,
/// so call sites pass closures naturally:
///
/// ```ignore
/// PluginAdapter::new(MANIFEST)?
///     .on_tool(|inv: ToolInvocation| async move {
///         match inv.tool_name.as_str() {
///             "echo" => Ok(inv.args),
///             other => Err(ToolInvocationError::NotFound(other.into())),
///         }
///     })
///     .run_stdio().await
/// ```
pub trait ToolHandler: Send + Sync + 'static {
    /// Invoke the handler. Returning `Ok(Value)` becomes the
    /// `result` field of the JSON-RPC reply; `Err(...)` maps to
    /// `{ error: { code, message } }` in the
    /// `-33401..-33405` band.
    fn call(
        &self,
        invocation: ToolInvocation,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<serde_json::Value, ToolInvocationError>> + Send,
        >,
    >;
}

impl<F, Fut> ToolHandler for F
where
    F: Fn(ToolInvocation) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<serde_json::Value, ToolInvocationError>>
        + Send
        + 'static,
{
    fn call(
        &self,
        invocation: ToolInvocation,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<serde_json::Value, ToolInvocationError>> + Send,
        >,
    > {
        Box::pin((self)(invocation))
    }
}

/// Phase 81.17.c.ctx — tool dispatch context bundling host
/// resources the handler can reach. Designed to grow without
/// breaking the [`ToolHandlerWithContext`] signature: caller
/// pattern-matches the fields they need, ignores the rest.
///
/// Available today:
///   - `broker` — full `BrokerSender` for `publish` / `request`
///     / `complete_llm` / `recall_memory` from inside a tool.
///     Cheap to clone — internals are `Arc`-shared.
///   - `plugin_id` — manifest id (echoed for multi-plugin
///     handlers that dispatch by tool name).
///
/// Future fields land via field additions only. Plugin authors
/// who don't read them are immune.
#[non_exhaustive]
#[derive(Clone)]
pub struct ToolContext {
    /// Channel for outbound JSON-RPC frames (broker publish,
    /// LLM completion, memory recall). Holds the same writer
    /// the dispatch loop uses; concurrent clones serialise on
    /// the writer's `Mutex`.
    pub broker: BrokerSender,
    /// Stable plugin id pulled from the manifest. Tools
    /// matching on `<plugin_id>_*` names use this for
    /// validation; multi-plugin glue handlers route on it.
    pub plugin_id: String,
}

// `BrokerSender` carries trait-object fields (`Mutex<Box<dyn
// AsyncWrite>>`) that can't be `Debug`-derived. Hand-rolled
// formatter exposes `plugin_id` and redacts the rest so logs
// don't leak handle addresses.
impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("plugin_id", &self.plugin_id)
            .field("broker", &"<BrokerSender>")
            .finish()
    }
}

/// Phase 81.17.c.ctx — like [`ToolHandler`] but receives a
/// [`ToolContext`] alongside the [`ToolInvocation`]. Use this
/// variant when the tool body needs to publish / request /
/// LLM-call from the host. Register via
/// [`PluginAdapter::on_tool_with_context`] (mutually exclusive
/// with [`PluginAdapter::on_tool`]; the latter is preserved
/// for plugins that don't need the host channel).
///
/// Blanket-implemented for any
/// `Fn(ToolInvocation, ToolContext) -> Fut`:
///
/// ```ignore
/// PluginAdapter::new(MANIFEST)?
///     .on_tool_with_context(|inv, ctx| async move {
///         // notify operator via broker
///         let event = nexo_broker::Event::new(
///             "agent.email.notification.x", "my_plugin",
///             serde_json::json!({"hi": true}),
///         );
///         ctx.broker.publish("agent.email.notification.x", event).await.ok();
///         Ok(serde_json::json!({ "ok": true }))
///     })
///     .run_stdio().await
/// ```
pub trait ToolHandlerWithContext: Send + Sync + 'static {
    /// Invoke the handler. Same return-shape contract as
    /// [`ToolHandler::call`] — `Ok(Value)` becomes the
    /// JSON-RPC `result`; `Err(ToolInvocationError)` maps to
    /// the typed error band.
    fn call(
        &self,
        invocation: ToolInvocation,
        ctx: ToolContext,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<serde_json::Value, ToolInvocationError>> + Send,
        >,
    >;
}

impl<F, Fut> ToolHandlerWithContext for F
where
    F: Fn(ToolInvocation, ToolContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<serde_json::Value, ToolInvocationError>>
        + Send
        + 'static,
{
    fn call(
        &self,
        invocation: ToolInvocation,
        ctx: ToolContext,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<serde_json::Value, ToolInvocationError>> + Send,
        >,
    > {
        Box::pin((self)(invocation, ctx))
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
    /// Phase 81.17.c — tool defs advertised in the `initialize`
    /// reply's `tools: [...]` field. The host's decoder
    /// (`nexo_core::agent::tool_remote::RemoteToolDef`) consumes
    /// this list to register `RemoteToolHandler`s in the agent's
    /// scoped registry. Empty when the plugin doesn't expose
    /// tools — initialize-reply omits the field.
    declared_tools: Vec<ToolDef>,
    /// Phase 81.17.c — single dispatch closure invoked on every
    /// `tool.invoke` request. `None` when the plugin doesn't
    /// expose tools — `tool.invoke` requests reply `-32601 method
    /// not found` so the host's RemoteToolHandler surfaces a
    /// clear error.
    tool_handler: Option<Arc<dyn ToolHandler>>,
    /// Phase 81.17.c.ctx — like [`tool_handler`] but the
    /// closure also receives a [`ToolContext`] (broker access,
    /// plugin id). Mutually exclusive with `tool_handler`;
    /// when both are set the with-context handler wins
    /// (operator likely migrated incrementally + forgot to
    /// drop the old call).
    tool_handler_with_context: Option<Arc<dyn ToolHandlerWithContext>>,
}

/// Phase 81.15.c.b — handle returned by
/// [`BrokerSender::complete_llm_stream`]. Yields text chunks as
/// the host streams them, then a final [`LlmCompleteResult`] with
/// usage + finish reason after the stream closes.
///
/// Typical usage:
///
/// ```ignore
/// let mut stream = broker.complete_llm_stream(params).await?;
/// while let Some(chunk) = stream.next_chunk().await {
///     print!("{}", chunk);
/// }
/// let result = stream.await_final().await?;
/// println!("\n[finish_reason={}]", result.finish_reason);
/// ```
///
/// Dropping the `LlmStream` early is safe — the pending entry
/// is cleaned up via `Drop` so a late delta or final reply
/// from the host is silently discarded with a debug log on the
/// dispatch loop side.
pub struct LlmStream {
    request_id: u64,
    chunks: mpsc::UnboundedReceiver<String>,
    /// `Option` so [`Self::await_final`] can `take()` ownership
    /// of the receiver despite `LlmStream` having a `Drop` impl
    /// (which forbids moving fields out of `&mut self`).
    finished: Option<oneshot::Receiver<Result<LlmCompleteResult, RpcError>>>,
    pending: ChildPending,
}

impl LlmStream {
    /// Pull the next text chunk. Returns `None` when the stream
    /// closes (after which [`Self::await_final`] should be
    /// awaited for the final result).
    pub async fn next_chunk(&mut self) -> Option<String> {
        self.chunks.recv().await
    }

    /// Await the host's final response. Resolves once all
    /// deltas have been delivered and the host's response frame
    /// lands. Returns [`RpcError::Server`] when the host
    /// returned a JSON-RPC error (e.g. mid-stream provider
    /// failure mapped to `-32603`); [`RpcError::Transport`] when
    /// the dispatch loop dropped the oneshot before resolving
    /// (host crashed mid-stream). Calling twice returns
    /// `RpcError::Transport` on the second call (the receiver
    /// was already taken).
    pub async fn await_final(mut self) -> Result<LlmCompleteResult, RpcError> {
        let rx = self
            .finished
            .take()
            .ok_or_else(|| RpcError::Transport("await_final already consumed".into()))?;
        match rx.await {
            Ok(payload) => payload,
            Err(_canceled) => Err(RpcError::Transport(
                "final response oneshot canceled (host closed mid-stream)".into(),
            )),
        }
    }
}

impl Drop for LlmStream {
    fn drop(&mut self) {
        // Clean up the pending entry if it's still there. The
        // dispatch loop's get/remove on response path is the
        // normal cleanup; this Drop covers the case where the
        // user dropped the stream before consuming the final
        // reply (or before any deltas arrived). Late deltas /
        // final reply land on a missing pending entry → dropped
        // with debug log.
        self.pending.remove(&self.request_id);
    }
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
            declared_tools: Vec::new(),
            tool_handler: None,
            tool_handler_with_context: None,
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

    /// Phase 81.17.c — declare the tools this plugin will expose
    /// in its `initialize` reply. Each [`ToolDef::name`] MUST
    /// appear in the manifest's `[plugin.extends] tools = [...]`
    /// allowlist or the host kills the subprocess at handshake.
    ///
    /// Pair with [`Self::on_tool`] to handle invocations.
    pub fn declare_tools(mut self, defs: impl IntoIterator<Item = ToolDef>) -> Self {
        self.declared_tools = defs.into_iter().collect();
        self
    }

    /// Phase 81.17.c — register the dispatch handler for
    /// incoming `tool.invoke` requests. The handler matches on
    /// [`ToolInvocation::tool_name`] and routes to per-tool
    /// logic; returning `Ok(value)` becomes the JSON-RPC
    /// `result`, `Err(...)` maps to a `-33401..-33405` error.
    ///
    /// Without a handler, the dispatch loop replies `-32601
    /// method not found` to `tool.invoke` requests so the host's
    /// `RemoteToolHandler` surfaces a clear error.
    pub fn on_tool<H: ToolHandler>(mut self, handler: H) -> Self {
        self.tool_handler = Some(Arc::new(handler));
        self
    }

    /// Phase 81.17.c.ctx — same as [`Self::on_tool`] but the
    /// handler closure receives a [`ToolContext`] alongside
    /// the [`ToolInvocation`]. Use this when the tool body
    /// needs to publish to the broker, request via JSON-RPC,
    /// or call the host's LLM / memory APIs from inside the
    /// invocation.
    ///
    /// Mutually exclusive with `on_tool` — calling both during
    /// the builder chain is allowed (no panic), but the
    /// dispatch loop prefers the context-aware variant. Tests
    /// + linters can detect the latent bug; runtime accepts
    /// both for forward / backward migration ergonomics.
    pub fn on_tool_with_context<H: ToolHandlerWithContext>(mut self, handler: H) -> Self {
        self.tool_handler_with_context = Some(Arc::new(handler));
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
    let manifest_value = serde_json::to_value(&adapter.cached_manifest)
        .map_err(|e| SdkError::Io(io::Error::new(io::ErrorKind::Other, e.to_string())))?;
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
                    if let Some((_, kind)) = pending.remove(&req_id) {
                        let err_obj = frame.get("error").cloned();
                        let result_val = frame.get("result").cloned().unwrap_or(Value::Null);
                        match kind {
                            PendingKind::Single(sender) => {
                                let payload = if let Some(err) = err_obj {
                                    let code =
                                        err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603)
                                            as i32;
                                    let message = err
                                        .get("message")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("(no message)")
                                        .to_string();
                                    Err(RpcError::Server { code, message })
                                } else {
                                    Ok(result_val)
                                };
                                let _ = sender.send(payload);
                            }
                            PendingKind::Streaming { final_tx, .. } => {
                                // Phase 81.15.c.b — final response
                                // for a streaming request. delta_tx
                                // drops with the enum, closing the
                                // chunks channel cleanly so the
                                // user's `next_chunk()` loop returns
                                // `None`. Then `await_final()`
                                // resolves with this payload.
                                let payload = if let Some(err) = err_obj {
                                    let code =
                                        err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603)
                                            as i32;
                                    let message = err
                                        .get("message")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("(no message)")
                                        .to_string();
                                    Err(RpcError::Server { code, message })
                                } else {
                                    serde_json::from_value::<LlmCompleteResult>(result_val).map_err(
                                        |e| {
                                            RpcError::Decode(format!(
                                                "llm.complete stream final result: {e}"
                                            ))
                                        },
                                    )
                                };
                                let _ = final_tx.send(payload);
                            }
                        }
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
            } else if method == "llm.complete.delta" {
                // Phase 81.15.c.b — streaming chunk for an
                // outstanding `complete_llm_stream` request. Look
                // up the pending entry by request_id; if it's
                // Streaming, push the chunk into delta_tx. If the
                // pending entry is missing (already finalized or
                // user dropped the LlmStream), the chunk is
                // dropped with debug log.
                let req_id = params.get("request_id").and_then(|v| v.as_u64());
                let chunk = params.get("chunk").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(req_id) = req_id {
                    if let Some(entry) = pending.get(&req_id) {
                        if let PendingKind::Streaming { delta_tx, .. } = entry.value() {
                            let _ = delta_tx.send(chunk.to_string());
                        } else {
                            tracing::debug!(
                                request_id = req_id,
                                "llm.complete.delta arrived for non-streaming pending — drop"
                            );
                        }
                    } else {
                        tracing::debug!(
                            request_id = req_id,
                            "llm.complete.delta with unknown request_id — drop"
                        );
                    }
                }
            } else {
                tracing::debug!(method, "unhandled notification — drop");
            }
            continue;
        }

        match method {
            "initialize" => {
                // Phase 81.17.c — when the plugin declared tools,
                // emit them as `tools: [...]` so the host's
                // `Inner.declared_tools` (subprocess.rs:1052)
                // populates and `register_remote_tool_handlers_after_init`
                // can register `RemoteToolHandler`s.
                let mut result_obj = json!({
                    "manifest": manifest_value,
                    "server_version": adapter.server_version,
                });
                if !adapter.declared_tools.is_empty() {
                    let tools_value =
                        serde_json::to_value(&adapter.declared_tools).map_err(|e| {
                            SdkError::Io(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("declared_tools serialise failed: {e}"),
                            ))
                        })?;
                    if let Some(map) = result_obj.as_object_mut() {
                        map.insert("tools".to_string(), tools_value);
                    }
                }
                write_result(&writer, id, result_obj).await?;
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
            "tool.invoke" => {
                // Phase 81.17.c — host-initiated tool dispatch. No
                // registered handler ⇒ reply with -32601 so the
                // host's `RemoteToolHandler` surfaces a typed
                // ToolError to the agent.
                //
                // Phase 81.17.c.ctx — the context-aware handler
                // (`on_tool_with_context`) is preferred when both
                // are registered; falls back to the plain
                // `on_tool` handler. No handler at all → -32601.
                let with_ctx = adapter.tool_handler_with_context.clone();
                let plain = adapter.tool_handler.clone();
                if with_ctx.is_none() && plain.is_none() {
                    write_error(
                        &writer,
                        id,
                        -32601,
                        "method not found: tool.invoke (no handler registered — call PluginAdapter::on_tool or on_tool_with_context)",
                    )
                    .await?;
                    continue;
                }
                let params = frame.get("params").cloned().unwrap_or(Value::Null);
                let invocation: ToolInvocation = match serde_json::from_value(params) {
                    Ok(inv) => inv,
                    Err(e) => {
                        write_error(
                            &writer,
                            id,
                            -32602,
                            &format!("tool.invoke: invalid params: {e}"),
                        )
                        .await?;
                        continue;
                    }
                };
                let result = if let Some(handler) = with_ctx {
                    let ctx = ToolContext {
                        broker: BrokerSender {
                            writer: writer.clone(),
                            pending: pending.clone(),
                            next_id: next_id.clone(),
                        },
                        plugin_id: adapter.cached_manifest.plugin.id.clone(),
                    };
                    handler.call(invocation, ctx).await
                } else {
                    plain.expect("checked above").call(invocation).await
                };
                match result {
                    Ok(value) => {
                        write_result(&writer, id, value).await?;
                    }
                    Err(err) => {
                        let code = err.code();
                        write_error(&writer, id, code, &err.to_string()).await?;
                    }
                }
            }
            other => {
                write_error(&writer, id, -32601, &format!("method not found: {other}")).await?;
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

    // ── Phase 81.17.c — tool dispatch types ────────────────────

    #[test]
    fn tool_def_serde_round_trip() {
        let def = ToolDef {
            name: "test_plugin_echo".into(),
            description: "Echo the args back.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "msg": { "type": "string" } },
                "required": ["msg"],
            }),
        };
        let s = serde_json::to_string(&def).unwrap();
        // Wire-shape sentinel: must match the host-side decoder
        // (`nexo_core::agent::tool_remote::RemoteToolDef`).
        assert!(s.contains("\"name\":\"test_plugin_echo\""));
        assert!(s.contains("\"description\":\"Echo the args back.\""));
        assert!(s.contains("\"input_schema\""));
        let back: ToolDef = serde_json::from_str(&s).unwrap();
        assert_eq!(back.name, def.name);
        assert_eq!(back.description, def.description);
        assert_eq!(back.input_schema, def.input_schema);
    }

    #[test]
    fn tool_invocation_args_default_to_null() {
        let raw = r#"{ "plugin_id": "p", "tool_name": "t" }"#;
        let inv: ToolInvocation = serde_json::from_str(raw).unwrap();
        assert_eq!(inv.plugin_id, "p");
        assert_eq!(inv.tool_name, "t");
        assert_eq!(inv.args, serde_json::Value::Null);
        assert!(inv.agent_id.is_none());
    }

    #[test]
    fn tool_invocation_full_shape_round_trip() {
        let raw = r#"{
            "plugin_id": "browser",
            "tool_name": "browser_navigate",
            "args": { "url": "about:blank" },
            "agent_id": "ana"
        }"#;
        let inv: ToolInvocation = serde_json::from_str(raw).unwrap();
        assert_eq!(inv.tool_name, "browser_navigate");
        assert_eq!(inv.args["url"], "about:blank");
        assert_eq!(inv.agent_id.as_deref(), Some("ana"));
    }

    #[test]
    fn tool_invocation_error_codes_match_contract_v1_10_band() {
        assert_eq!(ToolInvocationError::NotFound("x".into()).code(), -33401);
        assert_eq!(
            ToolInvocationError::ArgumentInvalid("x".into()).code(),
            -33402
        );
        assert_eq!(
            ToolInvocationError::ExecutionFailed("x".into()).code(),
            -33403
        );
        assert_eq!(ToolInvocationError::Unavailable("x".into()).code(), -33404);
        assert_eq!(ToolInvocationError::Denied("x".into()).code(), -33405);
    }

    #[test]
    fn tool_invocation_error_messages_format_with_payload() {
        let e = ToolInvocationError::NotFound("browser_thirteenth".into());
        assert_eq!(e.to_string(), "tool not found: browser_thirteenth");
        let e = ToolInvocationError::ExecutionFailed("CDP 500".into());
        assert_eq!(e.to_string(), "execution failed: CDP 500");
    }

    #[tokio::test]
    async fn tool_handler_blanket_impl_accepts_closure() {
        // The closure form is the canonical entry point — verify
        // that an `impl Fn(ToolInvocation) -> Fut` satisfies the
        // `ToolHandler` trait via the blanket impl, and that the
        // dispatch routes args through unchanged.
        let handler = |inv: ToolInvocation| async move {
            match inv.tool_name.as_str() {
                "echo" => Ok(inv.args),
                other => Err(ToolInvocationError::NotFound(other.into())),
            }
        };
        let inv = ToolInvocation {
            plugin_id: "p".into(),
            tool_name: "echo".into(),
            args: serde_json::json!({"hello": "world"}),
            agent_id: None,
        };
        let out = ToolHandler::call(&handler, inv).await.unwrap();
        assert_eq!(out, serde_json::json!({"hello": "world"}));
    }

    #[tokio::test]
    async fn tool_handler_blanket_impl_propagates_error_variant() {
        let handler = |_inv: ToolInvocation| async move {
            Err::<serde_json::Value, _>(ToolInvocationError::Denied("nope".into()))
        };
        let inv = ToolInvocation {
            plugin_id: "p".into(),
            tool_name: "x".into(),
            args: serde_json::Value::Null,
            agent_id: None,
        };
        let err = ToolHandler::call(&handler, inv).await.unwrap_err();
        assert_eq!(err.code(), -33405);
    }

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

    // ── Phase 81.17.c — initialize-reply tools + tool.invoke routing ──

    #[tokio::test]
    async fn initialize_reply_omits_tools_when_none_declared() {
        // Default builder: no `.declare_tools(...)` call; the
        // initialize reply must NOT carry a `tools` field so the
        // host's `result.pointer("/tools")` returns None and
        // `Inner.declared_tools` stays empty.
        let adapter = PluginAdapter::new(TEST_MANIFEST).expect("manifest parses");
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert!(
            reply["result"].get("tools").is_none(),
            "expected no `tools` field; got: {}",
            reply["result"]
        );
    }

    #[tokio::test]
    async fn initialize_reply_includes_declared_tools_array() {
        let defs = vec![
            ToolDef {
                name: "test_plugin_echo".into(),
                description: "Echo args.".into(),
                input_schema: serde_json::json!({"type":"object"}),
            },
            ToolDef {
                name: "test_plugin_ping".into(),
                description: "Ping/pong.".into(),
                input_schema: serde_json::json!({"type":"object"}),
            },
        ];
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .declare_tools(defs);
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        let tools = reply["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "test_plugin_echo");
        assert_eq!(tools[1]["name"], "test_plugin_ping");
    }

    #[tokio::test]
    async fn tool_invoke_routes_to_registered_handler() {
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_tool(
                |inv: ToolInvocation| async move { Ok(serde_json::json!({"echoed": inv.args})) },
            );
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(
                br#"{"jsonrpc":"2.0","id":7,"method":"tool.invoke","params":{"plugin_id":"test_plugin","tool_name":"echo","args":{"x":1}}}
"#,
            )
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["echoed"]["x"], 1);
    }

    #[tokio::test]
    async fn tool_invoke_handler_error_maps_to_minus_33401() {
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_tool(|inv: ToolInvocation| async move {
                Err::<serde_json::Value, _>(ToolInvocationError::NotFound(inv.tool_name))
            });
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(
                br#"{"jsonrpc":"2.0","id":8,"method":"tool.invoke","params":{"plugin_id":"test_plugin","tool_name":"unknown"}}
"#,
            )
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["id"], 8);
        assert_eq!(reply["error"]["code"], -33401);
        assert!(reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown"));
    }

    #[tokio::test]
    async fn tool_invoke_without_handler_returns_method_not_found() {
        // No `.on_tool(...)` call; dispatch loop must reply
        // -32601 (method not found) so the host's
        // RemoteToolHandler surfaces a typed error rather than
        // hanging on a never-resolved oneshot.
        let adapter = PluginAdapter::new(TEST_MANIFEST).expect("manifest parses");
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(
                br#"{"jsonrpc":"2.0","id":9,"method":"tool.invoke","params":{"plugin_id":"test_plugin","tool_name":"x"}}
"#,
            )
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["id"], 9);
        assert_eq!(reply["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn tool_invoke_with_context_handler_receives_broker_and_plugin_id() {
        // Phase 81.17.c.ctx — context-aware handler should
        // receive the manifest's plugin_id + a working
        // BrokerSender. Asserts the plugin_id surfaces in the
        // reply so the dispatch path is correct.
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_tool_with_context(|inv: ToolInvocation, ctx: ToolContext| async move {
                Ok(serde_json::json!({
                    "echoed_plugin_id": ctx.plugin_id,
                    "tool_name": inv.tool_name,
                }))
            });
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(
                br#"{"jsonrpc":"2.0","id":10,"method":"tool.invoke","params":{"plugin_id":"test_plugin","tool_name":"ping"}}
"#,
            )
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["id"], 10);
        assert_eq!(reply["result"]["echoed_plugin_id"], "test_plugin");
        assert_eq!(reply["result"]["tool_name"], "ping");
    }

    #[tokio::test]
    async fn tool_invoke_with_context_takes_precedence_over_plain() {
        // When both `on_tool` and `on_tool_with_context` are
        // registered, dispatch loop prefers the context-aware
        // variant — verifies the precedence rule documented in
        // the builder doc-comment.
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_tool(|_inv: ToolInvocation| async move { Ok(serde_json::json!({"path": "plain"})) })
            .on_tool_with_context(|_inv, _ctx| async move {
                Ok(serde_json::json!({"path": "with_context"}))
            });
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(
                br#"{"jsonrpc":"2.0","id":11,"method":"tool.invoke","params":{"plugin_id":"test_plugin","tool_name":"x"}}
"#,
            )
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["result"]["path"], "with_context");
    }

    #[tokio::test]
    async fn broker_event_dispatches_to_user_handler() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(move |topic: String, event: Event, _broker: BrokerSender| {
                let called = called_clone.clone();
                async move {
                    assert_eq!(topic, "plugin.outbound.test");
                    assert_eq!(event.source, "host");
                    called.store(true, Ordering::SeqCst);
                }
            });
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
            .on_broker_event(move |_topic, _event, broker: BrokerSender| {
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
            });
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

    /// Phase 81.15.c.b — `complete_llm_stream` returns an
    /// `LlmStream` yielding text chunks via `next_chunk()` and
    /// resolving `await_final()` once the host sends the final
    /// response frame. Test fabricates 3 deltas + a final
    /// response, asserts the handler reassembled the text and
    /// got the right finish_reason + usage.
    #[tokio::test]
    async fn complete_llm_stream_yields_chunks_and_final_result() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let observed = Arc::new(AtomicBool::new(false));
        let observed_clone = observed.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(move |_t, _e, broker: BrokerSender| {
                let observed = observed_clone.clone();
                async move {
                    let params = LlmCompleteParams {
                        provider: "stub".into(),
                        model: "x".into(),
                        messages: vec![],
                        ..Default::default()
                    };
                    let mut stream = match broker.complete_llm_stream(params).await {
                        Ok(s) => s,
                        Err(e) => panic!("complete_llm_stream open failed: {e}"),
                    };
                    let mut assembled = String::new();
                    while let Some(chunk) = stream.next_chunk().await {
                        assembled.push_str(&chunk);
                    }
                    let result = match stream.await_final().await {
                        Ok(r) => r,
                        Err(e) => panic!("await_final failed: {e}"),
                    };
                    assert_eq!(assembled, "hello world");
                    assert_eq!(result.finish_reason, "stop");
                    assert_eq!(result.usage.completion_tokens, 5);
                    observed.store(true, Ordering::SeqCst);
                }
            });
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;

        // Trigger the handler via broker.event.
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

        // Adapter issues llm.complete with stream:true; capture
        // the request id then send 3 delta notifications + final
        // response.
        let req = read_reply_line(&mut host_read).await;
        assert_eq!(req["method"], "llm.complete");
        assert_eq!(req["params"]["stream"], true);
        let req_id = req["id"].clone();
        for chunk in ["hello", " ", "world"] {
            let delta = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "llm.complete.delta",
                "params": { "request_id": req_id, "chunk": chunk }
            });
            host_write
                .write_all(format!("{}\n", delta).as_bytes())
                .await
                .unwrap();
        }
        // After deltas land, send final response. The dispatch
        // loop dropping the Streaming pending entry closes
        // delta_tx → next_chunk() returns None → handler proceeds
        // to await_final.
        let final_resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "content": "",
                "finish_reason": "stop",
                "usage": { "prompt_tokens": 3, "completion_tokens": 5 }
            }
        });
        host_write
            .write_all(format!("{}\n", final_resp).as_bytes())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            observed.load(Ordering::SeqCst),
            "handler must reassemble chunks + observe final result"
        );
    }
}
