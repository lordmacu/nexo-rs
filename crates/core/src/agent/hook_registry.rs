//! Phase 11.6 — per-agent registry for extension-provided lifecycle hooks.
//!
//! Hooks are fired sequentially in registration order. A `before_*` hook
//! returning `abort: true` short-circuits further handlers and bubbles up as
//! `HookOutcome::Aborted` so the caller can skip the host event. `after_*`
//! hooks return `HookOutcome::Continue` regardless — their response is
//! advisory.
//!
//! Extension handler errors (timeout, transport, RPC error) are logged and
//! treated as `Continue`. Philosophy: extension misbehavior must not take
//! down agent flow.
use agent_extensions::HookResponse;
use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
#[async_trait]
pub trait HookHandler: Send + Sync {
    async fn on_hook(&self, name: &str, event: Value) -> anyhow::Result<HookResponse>;
}
#[derive(Debug, Clone, PartialEq)]
pub enum HookOutcome {
    Continue,
    Aborted {
        plugin_id: String,
        reason: Option<String>,
    },
}
/// A hook handler entry. `priority` is taken from the owning extension's
/// `plugin.priority` field (default `0`); lower values fire first.
#[derive(Clone)]
struct HandlerEntry {
    plugin_id: String,
    priority: i32,
    /// Insertion order within the same priority class — preserves
    /// deterministic ordering when two extensions share a priority.
    seq: u64,
    handler: Arc<dyn HookHandler>,
}

#[derive(Clone, Default)]
pub struct HookRegistry {
    handlers: Arc<DashMap<String, Vec<HandlerEntry>>>,
    next_seq: Arc<std::sync::atomic::AtomicU64>,
}
impl std::fmt::Debug for HookRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRegistry")
            .field("handler_count", &self.handlers.len())
            .finish()
    }
}
impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(
        &self,
        hook_name: &str,
        plugin_id: impl Into<String>,
        handler: impl HookHandler + 'static,
    ) {
        self.register_with_priority(hook_name, plugin_id, 0, handler);
    }

    /// Register a handler with an explicit firing priority — lower
    /// values fire first. Ties are broken by registration order (seq).
    pub fn register_with_priority(
        &self,
        hook_name: &str,
        plugin_id: impl Into<String>,
        priority: i32,
        handler: impl HookHandler + 'static,
    ) {
        let plugin_id = plugin_id.into();
        let seq = self
            .next_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut entry = self.handlers.entry(hook_name.to_string()).or_default();
        // Defensive cap — a buggy extension that re-registers hooks on
        // every event would otherwise grow the Vec without bound and
        // blow up the hot path. 128 is far above any realistic extension
        // count (typical deployments have <10 registered per hook).
        const MAX_HANDLERS_PER_HOOK: usize = 128;
        if entry.len() >= MAX_HANDLERS_PER_HOOK {
            tracing::warn!(
                hook = %hook_name,
                plugin = %plugin_id,
                count = entry.len(),
                cap = MAX_HANDLERS_PER_HOOK,
                "refusing hook registration: cap reached"
            );
            return;
        }
        entry.push(HandlerEntry {
            plugin_id,
            priority,
            seq,
            handler: Arc::new(handler),
        });
        entry.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.seq.cmp(&b.seq)));
    }

    /// Drop every handler registered under `plugin_id` across all
    /// hooks. Used on extension reload / unload so the caller can
    /// re-register a fresh handler without the old one still firing.
    /// Returns the number of handler entries removed.
    pub fn deregister_plugin(&self, plugin_id: &str) -> usize {
        let mut removed = 0usize;
        for mut entry in self.handlers.iter_mut() {
            let before = entry.len();
            entry.retain(|h| h.plugin_id != plugin_id);
            removed += before - entry.len();
        }
        removed
    }
    pub fn handler_count(&self, hook_name: &str) -> usize {
        self.handlers.get(hook_name).map(|v| v.len()).unwrap_or(0)
    }
    /// Fire all handlers for `hook_name` in registration order. A handler
    /// returning `abort: true` short-circuits and returns `Aborted`. Handler
    /// errors are logged and treated as `Continue`.
    pub async fn fire(&self, hook_name: &str, event: Value) -> HookOutcome {
        let mut ev = event;
        self.fire_with_merge(hook_name, &mut ev).await
    }

    /// Same as [`fire`] but threads the event through handlers: each
    /// handler that returns a non-null JSON-object `override` has its
    /// keys shallow-merged onto `event` before the next handler sees
    /// it. On return, `event` contains the final merged state. An
    /// aborted handler's `override` is discarded (abort wins).
    pub async fn fire_with_merge(&self, hook_name: &str, event: &mut Value) -> HookOutcome {
        let handlers: Vec<HandlerEntry> = match self.handlers.get(hook_name) {
            Some(v) => v.iter().cloned().collect(),
            None => return HookOutcome::Continue,
        };
        let advisory = hook_name.starts_with("after_");
        for HandlerEntry {
            plugin_id, handler, ..
        } in handlers
        {
            match handler.on_hook(hook_name, event.clone()).await {
                Ok(resp) => {
                    if resp.abort {
                        if advisory {
                            tracing::warn!(
                                hook = %hook_name,
                                ext = %plugin_id,
                                reason = resp.reason.as_deref().unwrap_or("<none>"),
                                "after_* hook returned abort=true; ignored (after hooks are advisory)",
                            );
                            continue;
                        }
                        if resp.reason.is_none() {
                            tracing::warn!(
                                hook = %hook_name,
                                ext = %plugin_id,
                                "hook aborted without reason",
                            );
                        }
                        return HookOutcome::Aborted {
                            plugin_id,
                            reason: resp.reason,
                        };
                    }
                    if let Some(patch) = resp.override_event {
                        apply_event_override(event, patch, hook_name, &plugin_id);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        hook = %hook_name,
                        ext = %plugin_id,
                        error = %e,
                        "hook handler failed; treating as continue",
                    );
                }
            }
        }
        HookOutcome::Continue
    }
}

/// Shallow-merge `patch` onto `event`. Both must be JSON objects;
/// non-object `patch` is logged and dropped.
fn apply_event_override(event: &mut Value, patch: Value, hook: &str, ext: &str) {
    let Some(patch_obj) = patch.as_object() else {
        tracing::warn!(
            hook = %hook,
            ext = %ext,
            "hook override is not a JSON object; ignored"
        );
        return;
    };
    let Some(ev_obj) = event.as_object_mut() else {
        tracing::warn!(
            hook = %hook,
            ext = %ext,
            "hook event is not a JSON object; cannot merge override"
        );
        return;
    };
    for (k, v) in patch_obj {
        ev_obj.insert(k.clone(), v.clone());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct RecordingHook {
        responded: AtomicUsize,
        resp: HookResponse,
        fail: bool,
    }
    impl RecordingHook {
        fn ok(resp: HookResponse) -> Self {
            Self {
                responded: AtomicUsize::new(0),
                resp,
                fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                responded: AtomicUsize::new(0),
                resp: HookResponse::default(),
                fail: true,
            }
        }
        fn count(&self) -> usize {
            self.responded.load(Ordering::SeqCst)
        }
    }
    #[async_trait]
    impl HookHandler for RecordingHook {
        async fn on_hook(&self, _name: &str, _event: Value) -> anyhow::Result<HookResponse> {
            self.responded.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(anyhow::anyhow!("boom"))
            } else {
                Ok(self.resp.clone())
            }
        }
    }
    #[tokio::test]
    async fn fire_with_no_handlers_returns_continue() {
        let reg = HookRegistry::new();
        let out = reg.fire("before_message", serde_json::json!({})).await;
        assert_eq!(out, HookOutcome::Continue);
    }
    #[tokio::test]
    async fn fire_runs_handlers_in_registration_order() {
        let reg = HookRegistry::new();
        let h1 = Arc::new(RecordingHook::ok(HookResponse::default()));
        let h2 = Arc::new(RecordingHook::ok(HookResponse::default()));
        struct Delegate(Arc<RecordingHook>);
        #[async_trait]
        impl HookHandler for Delegate {
            async fn on_hook(&self, n: &str, e: Value) -> anyhow::Result<HookResponse> {
                self.0.on_hook(n, e).await
            }
        }
        reg.register("before_message", "a", Delegate(h1.clone()));
        reg.register("before_message", "b", Delegate(h2.clone()));
        assert_eq!(reg.handler_count("before_message"), 2);
        let out = reg.fire("before_message", serde_json::json!({})).await;
        assert_eq!(out, HookOutcome::Continue);
        assert_eq!(h1.count(), 1);
        assert_eq!(h2.count(), 1);
    }
    #[tokio::test]
    async fn first_abort_short_circuits() {
        let reg = HookRegistry::new();
        let h1 = Arc::new(RecordingHook::ok(HookResponse {
            abort: true,
            reason: Some("nope".into()),
            override_event: None,
        }));
        let h2 = Arc::new(RecordingHook::ok(HookResponse::default()));
        struct Delegate(Arc<RecordingHook>);
        #[async_trait]
        impl HookHandler for Delegate {
            async fn on_hook(&self, n: &str, e: Value) -> anyhow::Result<HookResponse> {
                self.0.on_hook(n, e).await
            }
        }
        reg.register("before_tool_call", "blocker", Delegate(h1.clone()));
        reg.register("before_tool_call", "later", Delegate(h2.clone()));
        let out = reg.fire("before_tool_call", serde_json::json!({})).await;
        match out {
            HookOutcome::Aborted { plugin_id, reason } => {
                assert_eq!(plugin_id, "blocker");
                assert_eq!(reason.as_deref(), Some("nope"));
            }
            other => panic!("expected abort, got {:?}", other),
        }
        assert_eq!(h1.count(), 1);
        assert_eq!(h2.count(), 0, "second handler must not run after abort");
    }
    #[tokio::test]
    async fn handler_error_is_logged_treated_as_continue() {
        let reg = HookRegistry::new();
        reg.register("before_message", "broken", RecordingHook::failing());
        let out = reg.fire("before_message", serde_json::json!({})).await;
        assert_eq!(out, HookOutcome::Continue);
    }
    #[test]
    fn handler_count_zero_for_unregistered() {
        let reg = HookRegistry::new();
        assert_eq!(reg.handler_count("after_message"), 0);
    }
    #[tokio::test]
    async fn after_hook_abort_is_ignored_and_continues() {
        let reg = HookRegistry::new();
        let h1 = Arc::new(RecordingHook::ok(HookResponse {
            abort: true,
            reason: Some("no-op".into()),
            override_event: None,
        }));
        let h2 = Arc::new(RecordingHook::ok(HookResponse::default()));
        struct Delegate(Arc<RecordingHook>);
        #[async_trait]
        impl HookHandler for Delegate {
            async fn on_hook(&self, n: &str, e: Value) -> anyhow::Result<HookResponse> {
                self.0.on_hook(n, e).await
            }
        }
        reg.register("after_tool_call", "first", Delegate(h1.clone()));
        reg.register("after_tool_call", "second", Delegate(h2.clone()));
        let out = reg.fire("after_tool_call", serde_json::json!({})).await;
        assert_eq!(out, HookOutcome::Continue, "after_* abort must be ignored");
        assert_eq!(h1.count(), 1);
        assert_eq!(
            h2.count(),
            1,
            "second handler still runs after after_* abort"
        );
    }

    #[tokio::test]
    async fn priority_orders_fire_low_first() {
        use std::sync::Mutex;
        // Captures the order handlers were called in.
        let order = Arc::new(Mutex::new(Vec::<String>::new()));

        struct OrderHook {
            who: String,
            order: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait]
        impl HookHandler for OrderHook {
            async fn on_hook(&self, _n: &str, _e: Value) -> anyhow::Result<HookResponse> {
                self.order.lock().unwrap().push(self.who.clone());
                Ok(HookResponse::default())
            }
        }

        let reg = HookRegistry::new();
        // Registered in "wrong" order (high priority first) — fire
        // must still call `security` before `logger`.
        reg.register_with_priority(
            "before_message",
            "logger",
            10,
            OrderHook {
                who: "logger".into(),
                order: order.clone(),
            },
        );
        reg.register_with_priority(
            "before_message",
            "security",
            -5,
            OrderHook {
                who: "security".into(),
                order: order.clone(),
            },
        );
        reg.register_with_priority(
            "before_message",
            "audit",
            10, // ties → insertion order within class
            OrderHook {
                who: "audit".into(),
                order: order.clone(),
            },
        );

        reg.fire("before_message", serde_json::json!({})).await;

        let got = order.lock().unwrap().clone();
        assert_eq!(got, vec!["security", "logger", "audit"]);
    }

    #[tokio::test]
    async fn override_event_merges_into_next_handler() {
        // h1 returns `{override: {text: "rewritten"}}`; h2 receives
        // the merged event and records it.
        use std::sync::Mutex;
        let seen = Arc::new(Mutex::new(Value::Null));

        struct OverrideHook;
        #[async_trait]
        impl HookHandler for OverrideHook {
            async fn on_hook(&self, _n: &str, _e: Value) -> anyhow::Result<HookResponse> {
                Ok(HookResponse {
                    abort: false,
                    reason: None,
                    override_event: Some(serde_json::json!({"text": "rewritten"})),
                })
            }
        }

        struct CaptureHook(Arc<Mutex<Value>>);
        #[async_trait]
        impl HookHandler for CaptureHook {
            async fn on_hook(&self, _n: &str, e: Value) -> anyhow::Result<HookResponse> {
                *self.0.lock().unwrap() = e;
                Ok(HookResponse::default())
            }
        }

        let reg = HookRegistry::new();
        reg.register("before_message", "rewriter", OverrideHook);
        reg.register("before_message", "capture", CaptureHook(seen.clone()));

        let mut event = serde_json::json!({
            "text": "original",
            "agent_id": "kate",
        });
        let out = reg.fire_with_merge("before_message", &mut event).await;
        assert_eq!(out, HookOutcome::Continue);

        // `capture` saw the merged event.
        let v = seen.lock().unwrap().clone();
        assert_eq!(v["text"], "rewritten");
        assert_eq!(v["agent_id"], "kate");

        // The caller sees the final merged event too.
        assert_eq!(event["text"], "rewritten");
        assert_eq!(event["agent_id"], "kate");
    }

    #[tokio::test]
    async fn deregister_plugin_drops_all_its_handlers() {
        struct Dummy;
        #[async_trait]
        impl HookHandler for Dummy {
            async fn on_hook(&self, _n: &str, _e: Value) -> anyhow::Result<HookResponse> {
                Ok(HookResponse::default())
            }
        }
        let reg = HookRegistry::new();
        reg.register("before_message", "alpha", Dummy);
        reg.register("after_message", "alpha", Dummy);
        reg.register("before_message", "beta", Dummy);
        assert_eq!(reg.handler_count("before_message"), 2);
        let removed = reg.deregister_plugin("alpha");
        assert_eq!(removed, 2);
        assert_eq!(reg.handler_count("before_message"), 1);
        assert_eq!(reg.handler_count("after_message"), 0);
    }

    #[test]
    fn registration_cap_refuses_after_128() {
        struct Dummy;
        #[async_trait]
        impl HookHandler for Dummy {
            async fn on_hook(&self, _n: &str, _e: Value) -> anyhow::Result<HookResponse> {
                Ok(HookResponse::default())
            }
        }
        let reg = HookRegistry::new();
        for i in 0..200 {
            reg.register("before_message", format!("ext{i}"), Dummy);
        }
        assert_eq!(reg.handler_count("before_message"), 128);
    }

    #[tokio::test]
    async fn override_event_ignored_when_abort() {
        // Aborted handler's override should not be applied.
        struct AbortOverride;
        #[async_trait]
        impl HookHandler for AbortOverride {
            async fn on_hook(&self, _n: &str, _e: Value) -> anyhow::Result<HookResponse> {
                Ok(HookResponse {
                    abort: true,
                    reason: Some("nope".into()),
                    override_event: Some(serde_json::json!({"text": "rewritten"})),
                })
            }
        }

        let reg = HookRegistry::new();
        reg.register("before_message", "blocker", AbortOverride);
        let mut event = serde_json::json!({"text": "original"});
        let out = reg.fire_with_merge("before_message", &mut event).await;
        assert!(matches!(out, HookOutcome::Aborted { .. }));
        assert_eq!(event["text"], "original", "abort must discard override");
    }
}
