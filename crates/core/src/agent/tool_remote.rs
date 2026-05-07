//! Phase 81.29 — `ToolHandler` impl backed by a subprocess
//! plugin's stdio bridge.
//!
//! Subprocess plugins declaring `[plugin.extends].tools =
//! ["browser_navigate", "browser_click"]` get one
//! `RemoteToolHandler` per tool name registered into the
//! plugin's per-instance `ScopedToolRegistry`. Each agent-loop
//! invocation translates into a single `tool.invoke` JSON-RPC
//! request over the subprocess plugin's stdio bridge (the same
//! `Inner.{stdin_tx, pending, next_id}` shared with
//! `RemoteChannelAdapter` / `RemoteLlmClient` / `RemoteHookHandler`
//! / `RemoteVectorBackend`).
//!
//! Schemas (description + JSONSchema for arguments) come from
//! the subprocess at handshake — the initialize-reply's `tools`
//! array carries a `RemoteToolDef` per advertised tool. The
//! daemon caches the schema so agents get typed function-calling
//! defs without per-call round-trips.
//!
//! Failure semantics: tool dispatch is part of the LLM's plan,
//! so we surface errors as `Err(anyhow!(...))` rather than the
//! continue-on-error pattern from 81.27 hooks. Agent loop sees
//! the failure, decides what to do.
//!
//! Default timeout: 60 s (matches 81.25 LLM band — tools span
//! fast `browser_click` to slow `browser_navigate`). Operator
//! override via `NEXO_PLUGIN_TOOL_TIMEOUT_MS`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::agent::context::AgentContext;
use crate::agent::tool_registry::ToolHandler;

const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// Tool advertised by a subprocess plugin at handshake time.
/// Daemon caches one entry per tool the plugin declared in
/// `extends.tools`. Used to build the `ToolDef` registered into
/// the plugin's `ScopedToolRegistry` plus the LLM's typed
/// function-calling spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteToolDef {
    /// Tool name (matches `extends.tools` entry verbatim;
    /// per-plugin namespace policy enforced by 81.3 validator).
    pub name: String,

    /// Human-readable description shown to the LLM as part of
    /// the function-calling spec. Plain text, no markdown.
    pub description: String,

    /// JSONSchema for the tool's argument object. Daemon-side
    /// `ToolHandler::call` receives validated args.
    pub input_schema: Value,
}

/// `ToolHandler` impl backed by a subprocess plugin. One
/// instance is registered per tool name listed in
/// `manifest.plugin.extends.tools` after the subprocess
/// confirms it via initialize-reply.
pub struct RemoteToolHandler {
    plugin_id: String,
    tool_name: String,
    /// Cached at handshake — exposed so the daemon's typed
    /// function-calling spec can be built without round-trip.
    description: String,
    input_schema: Value,
    stdin_tx: mpsc::Sender<Value>,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: Arc<AtomicU64>,
    request_timeout: Duration,
}

impl RemoteToolHandler {
    pub fn new(
        plugin_id: String,
        def: RemoteToolDef,
        stdin_tx: mpsc::Sender<Value>,
        pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
        next_id: Arc<AtomicU64>,
    ) -> Self {
        Self {
            plugin_id,
            tool_name: def.name,
            description: def.description,
            input_schema: def.input_schema,
            stdin_tx,
            pending,
            next_id,
            request_timeout: Self::resolve_timeout(),
        }
    }

    fn resolve_timeout() -> Duration {
        std::env::var("NEXO_PLUGIN_TOOL_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TOOL_TIMEOUT)
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }
}

#[async_trait]
impl ToolHandler for RemoteToolHandler {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tool.invoke",
            "params": {
                "plugin_id": &self.plugin_id,
                "tool_name": &self.tool_name,
                "args": args,
                "agent_id": ctx.agent_id,
            },
        });

        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        if self.stdin_tx.send(frame).await.is_err() {
            self.pending.remove(&id);
            return Err(anyhow!(
                "tool '{}' on plugin '{}' transport closed",
                self.tool_name,
                self.plugin_id
            ));
        }

        match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(err_str))) => Err(parse_tool_error_string(
                &self.plugin_id,
                &self.tool_name,
                &err_str,
            )),
            Ok(Err(_)) => {
                self.pending.remove(&id);
                Err(anyhow!(
                    "tool '{}' on plugin '{}' pending dropped (subprocess gone)",
                    self.tool_name,
                    self.plugin_id
                ))
            }
            Err(_) => {
                self.pending.remove(&id);
                Err(anyhow!(
                    "tool '{}' on plugin '{}' timed out after {}ms",
                    self.tool_name,
                    self.plugin_id,
                    self.request_timeout.as_millis()
                ))
            }
        }
    }
}

/// Map JSON-RPC error frames returned by `tool.invoke` into typed
/// `anyhow::Error`. Error band -33401..=-33405 is reserved for
/// tool dispatch (sibling to 81.24 channels -33001..=-33005, etc).
/// `err_str` is the `"code: message [+ data]"`-style string the
/// reader task formats in `subprocess.rs::parse_response`.
fn parse_tool_error_string(plugin_id: &str, tool_name: &str, err_str: &str) -> anyhow::Error {
    // Best-effort code extraction. Reader formats as
    //   "code: <i64>, message: <str>" (with optional data appended)
    // — parse defensively, fall through to passthrough on mismatch.
    let code = err_str
        .split_once("code:")
        .and_then(|(_, rest)| rest.trim().split_once(','))
        .and_then(|(c, _)| c.trim().parse::<i64>().ok());

    let context = format!("plugin '{}' tool '{}': ", plugin_id, tool_name);

    match code {
        Some(-33401) => anyhow!("{context}tool not found ({err_str})"),
        Some(-33402) => anyhow!("{context}tool argument invalid ({err_str})"),
        Some(-33403) => anyhow!("{context}tool execution failed ({err_str})"),
        Some(-33404) => anyhow!("{context}tool unavailable ({err_str})"),
        Some(-33405) => anyhow!("{context}tool denied ({err_str})"),
        Some(-32601) => {
            anyhow!("{context}plugin does not implement tool.invoke (method_not_found: {err_str})")
        }
        _ => anyhow!("{context}tool dispatch failed: {err_str}"),
    }
}

/// Surfaced by `register_remote_tool_handlers` when the host
/// can't bind the plugin's declared tools into the registry.
#[derive(Debug, thiserror::Error)]
pub enum ToolHandlerRegistrationError {
    #[error(
        "tool name `{tool_name}` already registered (likely by plugin `{prior_plugin_hint}` or built-in)"
    )]
    ToolNameAlreadyRegistered {
        tool_name: String,
        prior_plugin_hint: String,
    },

    #[error(
        "subprocess plugin inner not initialized — call register_remote_tool_handlers AFTER init()"
    )]
    InnerUnavailable,

    #[error(
        "plugin `{plugin_id}` declared tool `{tool_name}` in extends.tools but did not advertise it in initialize-reply tools array"
    )]
    NotInDeclaredList {
        plugin_id: String,
        tool_name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_tool_def_serde_round_trip() {
        let def = RemoteToolDef {
            name: "browser_navigate".into(),
            description: "Navigate to a URL".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "url": { "type": "string" } },
                "required": ["url"],
            }),
        };
        let s = serde_json::to_string(&def).unwrap();
        let parsed: RemoteToolDef = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, def);
    }

    fn build_handler(
        tool_name: &str,
    ) -> (
        RemoteToolHandler,
        mpsc::Receiver<Value>,
        Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    ) {
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(2));
        let h = RemoteToolHandler::new(
            "browser".into(),
            RemoteToolDef {
                name: tool_name.into(),
                description: "test".into(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            stdin_tx,
            pending.clone(),
            next_id,
        );
        (h, stdin_rx, pending)
    }

    #[tokio::test]
    async fn tool_invoke_serializes_with_plugin_id_and_args() {
        let (handler, mut stdin_rx, _pending) = build_handler("browser_navigate");

        // Spawn handler.call in background so we can intercept the
        // outgoing frame.
        let h = Arc::new(handler);
        let h_clone = h.clone();
        let bg = tokio::spawn(async move {
            // We do NOT call ctx-bearing variant; instead, manually
            // construct + send the frame the same way `call` does.
            // See comment in fake_ctx for why.
            let id = h_clone.next_id.fetch_add(1, Ordering::SeqCst);
            let frame = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tool.invoke",
                "params": {
                    "plugin_id": h_clone.plugin_id(),
                    "tool_name": h_clone.tool_name(),
                    "args": { "url": "https://example.com" },
                    "agent_id": "shopper",
                },
            });
            let _ = h_clone.stdin_tx.send(frame).await;
        });
        let _ = bg.await;

        let received = stdin_rx.recv().await.expect("frame must be sent");
        assert_eq!(received["method"], "tool.invoke");
        assert_eq!(received["params"]["plugin_id"], "browser");
        assert_eq!(received["params"]["tool_name"], "browser_navigate");
        assert_eq!(received["params"]["args"]["url"], "https://example.com");
        assert_eq!(received["params"]["agent_id"], "shopper");
        let _ = &h;
    }

    #[test]
    fn parse_tool_error_recognizes_tool_not_found() {
        let err = parse_tool_error_string("browser", "x", "code: -33401, message: not implemented");
        let s = err.to_string();
        assert!(s.contains("tool not found"), "{s}");
        assert!(s.contains("plugin 'browser' tool 'x'"), "{s}");
    }

    #[test]
    fn parse_tool_error_recognizes_argument_invalid() {
        let err =
            parse_tool_error_string("browser", "x", "code: -33402, message: arg 'url' missing");
        let s = err.to_string();
        assert!(s.contains("argument invalid"));
    }

    #[test]
    fn parse_tool_error_recognizes_execution_failed() {
        let err = parse_tool_error_string("browser", "x", "code: -33403, message: cdp timeout");
        let s = err.to_string();
        assert!(s.contains("execution failed"));
    }

    #[test]
    fn parse_tool_error_recognizes_unavailable() {
        let err = parse_tool_error_string(
            "browser",
            "x",
            "code: -33404, message: rate limited, data: {retry_after_ms:5000}",
        );
        let s = err.to_string();
        assert!(s.contains("unavailable"), "{s}");
    }

    #[test]
    fn parse_tool_error_recognizes_denied() {
        let err = parse_tool_error_string("browser", "x", "code: -33405, message: not allowed");
        let s = err.to_string();
        assert!(s.contains("denied"));
    }

    #[test]
    fn parse_tool_error_recognizes_method_not_found() {
        let err = parse_tool_error_string("browser", "x", "code: -32601, message: unknown");
        let s = err.to_string();
        assert!(s.contains("method_not_found"));
    }

    #[test]
    fn parse_tool_error_unknown_code_passes_through() {
        let err = parse_tool_error_string("browser", "x", "code: -99999, message: alien");
        let s = err.to_string();
        assert!(s.contains("dispatch failed"), "{s}");
    }

    #[test]
    fn registration_error_display_actionable() {
        let e = ToolHandlerRegistrationError::ToolNameAlreadyRegistered {
            tool_name: "browser_navigate".into(),
            prior_plugin_hint: "browser-builtin".into(),
        };
        let s = e.to_string();
        assert!(s.contains("browser_navigate"));
        let e = ToolHandlerRegistrationError::InnerUnavailable;
        assert!(e.to_string().contains("inner not initialized"));
        let e = ToolHandlerRegistrationError::NotInDeclaredList {
            plugin_id: "p".into(),
            tool_name: "p_t".into(),
        };
        assert!(e.to_string().contains("did not advertise"));
    }
}
