//! Phase 81.27 — `HookHandler` impl backed by a subprocess
//! plugin's stdio bridge.
//!
//! Subprocess plugins declaring `[plugin.extends].hooks =
//! ["before_message", "after_message"]` get one
//! `RemoteHookHandler` per hook name registered into the
//! daemon's `HookRegistry`. When the daemon fires that hook,
//! the handler translates the call into a `hook.on_hook`
//! JSON-RPC request over the subprocess plugin's stdio bridge
//! (the same `Inner.{stdin_tx, pending, next_id}` shared with
//! `RemoteChannelAdapter` and `RemoteLlmClient`).
//!
//! Continue-on-error: every dispatch failure (transport,
//! subprocess crash, timeout, malformed reply, JSON-RPC error)
//! returns `Ok(HookResponse::default())`. Reason: `HookRegistry`
//! must keep iterating handlers + agent flow must not break on
//! subprocess misbehavior. Errors land in `tracing::warn!` so
//! operators can debug without a hard failure.
//!
//! Hooks fire on the message hot path; default timeout is 5 s
//! (lower than 81.24's 30 s for channels and 81.25's 60 s for
//! LLMs). Operator override via `NEXO_PLUGIN_HOOK_TIMEOUT_MS`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use nexo_extensions::HookResponse;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::agent::hook_registry::HookHandler;

const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// `HookHandler` impl backed by a subprocess plugin. One
/// instance is registered per hook name listed in
/// `manifest.plugin.extends.hooks`.
pub struct RemoteHookHandler {
    hook_name: String,
    plugin_id: String,
    stdin_tx: mpsc::Sender<Value>,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: Arc<AtomicU64>,
    request_timeout: Duration,
}

impl RemoteHookHandler {
    pub fn new(
        hook_name: String,
        plugin_id: String,
        stdin_tx: mpsc::Sender<Value>,
        pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
        next_id: Arc<AtomicU64>,
    ) -> Self {
        Self {
            hook_name,
            plugin_id,
            stdin_tx,
            pending,
            next_id,
            request_timeout: Self::resolve_timeout(),
        }
    }

    fn resolve_timeout() -> Duration {
        std::env::var("NEXO_PLUGIN_HOOK_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_HOOK_TIMEOUT)
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn hook_name(&self) -> &str {
        &self.hook_name
    }
}

#[async_trait]
impl HookHandler for RemoteHookHandler {
    async fn on_hook(&self, name: &str, event: Value) -> anyhow::Result<HookResponse> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "hook.on_hook",
            "params": {
                "plugin_id": &self.plugin_id,
                "hook_name": name,
                "event": event,
            },
        });
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        if self.stdin_tx.send(frame).await.is_err() {
            self.pending.remove(&id);
            tracing::warn!(
                plugin = %self.plugin_id,
                hook = %self.hook_name,
                "hook.on_hook stdin send failed — Continue"
            );
            return Ok(HookResponse::default());
        }

        match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(Ok(value))) => match serde_json::from_value::<HookResponse>(value) {
                Ok(resp) => Ok(resp),
                Err(e) => {
                    tracing::warn!(
                        plugin = %self.plugin_id,
                        hook = %self.hook_name,
                        error = %e,
                        "hook.on_hook reply decode failed — Continue"
                    );
                    Ok(HookResponse::default())
                }
            },
            Ok(Ok(Err(err_str))) => {
                tracing::warn!(
                    plugin = %self.plugin_id,
                    hook = %self.hook_name,
                    error = %err_str,
                    "hook.on_hook returned error — Continue"
                );
                Ok(HookResponse::default())
            }
            Ok(Err(_)) => {
                self.pending.remove(&id);
                tracing::warn!(
                    plugin = %self.plugin_id,
                    hook = %self.hook_name,
                    "hook.on_hook pending dropped (subprocess gone) — Continue"
                );
                Ok(HookResponse::default())
            }
            Err(_) => {
                self.pending.remove(&id);
                tracing::warn!(
                    plugin = %self.plugin_id,
                    hook = %self.hook_name,
                    timeout_ms = self.request_timeout.as_millis() as u64,
                    "hook.on_hook timed out — Continue"
                );
                Ok(HookResponse::default())
            }
        }
    }
}

/// Errors surfaced by `register_remote_hook_handlers`.
/// `HookRegistry::register` itself never fails (cap-violations
/// log + skip silently), so the only failure mode here is
/// `Inner` not being initialized.
#[derive(Debug, thiserror::Error)]
pub enum HookHandlerRegistrationError {
    #[error("subprocess plugin inner not initialized — call register_remote_hook_handlers AFTER init()")]
    InnerUnavailable,
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn build() -> (
        Arc<RemoteHookHandler>,
        mpsc::Receiver<Value>,
        Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    ) {
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let handler = Arc::new(RemoteHookHandler::new(
            "before_message".to_string(),
            "mock_plugin".to_string(),
            stdin_tx,
            pending.clone(),
            next_id,
        ));
        (handler, stdin_rx, pending)
    }

    fn resolve_with_result(
        pending: &DashMap<u64, oneshot::Sender<Result<Value, String>>>,
        id: u64,
        result: Value,
    ) {
        if let Some((_, sender)) = pending.remove(&id) {
            let _ = sender.send(Ok(result));
        }
    }

    fn resolve_with_error(
        pending: &DashMap<u64, oneshot::Sender<Result<Value, String>>>,
        id: u64,
        err_obj: Value,
    ) {
        if let Some((_, sender)) = pending.remove(&id) {
            let _ = sender.send(Err(err_obj.to_string()));
        }
    }

    #[tokio::test]
    async fn on_hook_serializes_request_with_hook_name_and_event() {
        let (handler, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let handler = handler.clone();
            async move {
                handler
                    .on_hook("before_message", serde_json::json!({"sender":"alice"}))
                    .await
            }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        assert_eq!(frame["method"], "hook.on_hook");
        assert_eq!(frame["params"]["plugin_id"], "mock_plugin");
        assert_eq!(frame["params"]["hook_name"], "before_message");
        assert_eq!(frame["params"]["event"]["sender"], "alice");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(&pending, id, serde_json::json!({}));

        let resp = task.await.unwrap().unwrap();
        assert!(!resp.abort);
        assert_eq!(resp.decision, None);
    }

    #[tokio::test]
    async fn on_hook_deserializes_response_with_decision() {
        let (handler, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let handler = handler.clone();
            async move {
                handler
                    .on_hook("before_message", serde_json::json!({}))
                    .await
            }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(
            &pending,
            id,
            serde_json::json!({
                "abort": true,
                "reason": "PII detected",
                "decision": "block"
            }),
        );

        let resp = task.await.unwrap().unwrap();
        assert!(resp.abort);
        assert_eq!(resp.reason.as_deref(), Some("PII detected"));
        assert_eq!(resp.decision.as_deref(), Some("block"));
    }

    #[tokio::test]
    async fn on_hook_unsupported_method_returns_continue() {
        let (handler, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let handler = handler.clone();
            async move {
                handler
                    .on_hook("before_message", serde_json::json!({}))
                    .await
            }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_error(
            &pending,
            id,
            serde_json::json!({
                "code": -32601,
                "message": "hook.on_hook"
            }),
        );

        // Continue-on-error: returns Ok(default), NOT Err.
        let resp = task.await.unwrap().unwrap();
        assert_eq!(resp, HookResponse::default());
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn on_hook_timeout_returns_continue() {
        let (stdin_tx, mut stdin_rx) = mpsc::channel(8);
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let handler = RemoteHookHandler {
            hook_name: "before_message".into(),
            plugin_id: "mock_plugin".into(),
            stdin_tx,
            pending,
            next_id,
            request_timeout: Duration::from_millis(50),
        };

        let task = tokio::spawn(async move {
            handler
                .on_hook("before_message", serde_json::json!({}))
                .await
        });
        let _frame = stdin_rx.recv().await.expect("frame");
        // Don't resolve — let timeout fire.
        tokio::time::advance(Duration::from_millis(200)).await;

        let resp = task.await.unwrap().unwrap();
        assert_eq!(resp, HookResponse::default());
    }

    #[tokio::test]
    async fn on_hook_invalid_response_returns_continue() {
        let (handler, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let handler = handler.clone();
            async move {
                handler
                    .on_hook("before_message", serde_json::json!({}))
                    .await
            }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        // Reply with a value that doesn't deserialize to HookResponse
        // (e.g. an array — HookResponse expects an object).
        resolve_with_result(&pending, id, serde_json::json!([1, 2, 3]));

        let resp = task.await.unwrap().unwrap();
        assert_eq!(resp, HookResponse::default());
    }

    #[tokio::test]
    async fn on_hook_transform_decision_round_trips_transformed_body() {
        let (handler, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let handler = handler.clone();
            async move {
                handler
                    .on_hook(
                        "before_message",
                        serde_json::json!({"body": "ssn 123-45-6789"}),
                    )
                    .await
            }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(
            &pending,
            id,
            serde_json::json!({
                "decision": "transform",
                "transformed_body": "ssn [REDACTED]"
            }),
        );

        let resp = task.await.unwrap().unwrap();
        assert_eq!(resp.decision.as_deref(), Some("transform"));
        assert_eq!(
            resp.transformed_body.as_deref(),
            Some("ssn [REDACTED]")
        );
    }

    #[tokio::test]
    async fn on_hook_do_not_reply_again_round_trips() {
        let (handler, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let handler = handler.clone();
            async move {
                handler
                    .on_hook("after_message", serde_json::json!({}))
                    .await
            }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(
            &pending,
            id,
            serde_json::json!({
                "decision": "allow",
                "do_not_reply_again": true
            }),
        );

        let resp = task.await.unwrap().unwrap();
        assert!(resp.do_not_reply_again);
    }

    #[tokio::test]
    async fn on_hook_override_event_round_trips() {
        let (handler, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let handler = handler.clone();
            async move {
                handler
                    .on_hook("before_message", serde_json::json!({"k": "v"}))
                    .await
            }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(
            &pending,
            id,
            serde_json::json!({
                "override": { "k": "rewritten" }
            }),
        );

        let resp = task.await.unwrap().unwrap();
        assert_eq!(
            resp.override_event.as_ref().and_then(|v| v.get("k")).and_then(|v| v.as_str()),
            Some("rewritten")
        );
    }
}
