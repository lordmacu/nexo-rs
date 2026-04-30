//! Phase 80.9.b.b — channel-relayed permission decider.
//!
//! When a tool requires approval AND the binding has a registered
//! MCP channel server with `nexo/channel/permission` capability,
//! the relay decider:
//!
//! 1. Asks every eligible channel server for a verdict via
//!    `notifications/nexo/channel/permission_request`.
//! 2. Registers a pending entry in [`PendingPermissionMap`] for
//!    each request.
//! 3. **Races** every channel response against the inner
//!    decider's local decision via `tokio::select!`.
//! 4. The first responder wins; losers get cancelled cleanly so
//!    no side-channel state lingers.
//!
//! The local prompt always runs in parallel — phone approval is
//! a *second* surface, never a replacement. If every channel
//! server stays silent, the decision still flows through the
//! inner decider's normal path.
//!
//! Threading the binding id: the caller populates
//! `request.metadata.binding_id` (string) so the decorator knows
//! which `(binding, server)` rows to query in the channel
//! registry. When the metadata is absent or empty the decorator
//! short-circuits to the inner decider — no relay, no surprises.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_mcp::channel::SharedChannelRegistry;
use nexo_mcp::channel_permission::{
    short_request_id, truncate_input_preview, PendingPermissionMap, PermissionBehavior,
    PermissionRelayDispatcher, PermissionRequestParams, PermissionResponse,
    PERMISSION_REQUEST_SCHEMA_VERSION,
};

use crate::decider::PermissionDecider;
use crate::error::PermissionError;
use crate::types::{PermissionOutcome, PermissionRequest, PermissionResponse as DeciderResponse};

/// Metadata key that carries the binding id from the caller.
/// When absent the decorator skips relay entirely.
pub const METADATA_BINDING_ID: &str = "binding_id";

/// Decorator that races an inner [`PermissionDecider`] against
/// channel-relayed approval responses.
///
/// `D` is whatever decider produces the local-side decision —
/// typically the interactive prompt + audit pipeline. The
/// decorator never replaces it; channels are an *additional*
/// surface that can win the race when a human responds faster on
/// their phone than the local terminal.
pub struct ChannelRelayDecider<D> {
    inner: D,
    registry: SharedChannelRegistry,
    pending_map: Arc<PendingPermissionMap>,
    dispatcher: Arc<dyn PermissionRelayDispatcher>,
    /// Description rendered into the outbound prompt the channel
    /// server formats for the user. Defaults to a generic line —
    /// callers can override per-deployment.
    description_template: String,
}

impl<D> ChannelRelayDecider<D> {
    pub fn new(
        inner: D,
        registry: SharedChannelRegistry,
        pending_map: Arc<PendingPermissionMap>,
        dispatcher: Arc<dyn PermissionRelayDispatcher>,
    ) -> Self {
        Self {
            inner,
            registry,
            pending_map,
            dispatcher,
            description_template: "Approve this tool call?".into(),
        }
    }

    /// Override the description text every relay request carries.
    /// The server formats this onto the human's platform.
    pub fn with_description_template(mut self, template: impl Into<String>) -> Self {
        self.description_template = template.into();
        self
    }
}

/// Build a [`PermissionRequestParams`] from the decider's input.
/// Pure-fn so tests can exercise the wire shape without spinning
/// any infra. Exported so callers that want to emit a request
/// outside the decider chain can reuse the canonical shape.
pub fn build_request_params(
    request: &PermissionRequest,
    request_id: &str,
    description: &str,
) -> PermissionRequestParams {
    PermissionRequestParams {
        schema: PERMISSION_REQUEST_SCHEMA_VERSION,
        request_id: request_id.to_string(),
        tool_name: request.tool_name.clone(),
        description: description.to_string(),
        input_preview: truncate_input_preview(&request.input),
    }
}

/// Look up every channel server in `binding_id` that opted into
/// permission relay. Pure-fn so tests can stub the registry.
pub async fn eligible_relay_servers(
    registry: &SharedChannelRegistry,
    binding_id: &str,
) -> Vec<String> {
    registry
        .list_for_binding(binding_id)
        .await
        .into_iter()
        .filter(|r| r.permission_relay)
        .map(|r| r.server_name)
        .collect()
}

/// Map a [`PermissionResponse`] from the channel side onto the
/// decider's [`PermissionOutcome`]. `Allow` becomes `AllowOnce`;
/// `Deny` becomes a typed `Deny` with the responding server's
/// name in the rationale.
pub fn outcome_from_channel(response: &PermissionResponse) -> PermissionOutcome {
    match response.behavior {
        PermissionBehavior::Allow => PermissionOutcome::AllowOnce {
            updated_input: None,
        },
        PermissionBehavior::Deny => PermissionOutcome::Deny {
            message: format!(
                "denied via channel server '{}' (request_id={})",
                response.from_server, response.request_id
            ),
        },
    }
}

#[async_trait]
impl<D> PermissionDecider for ChannelRelayDecider<D>
where
    D: PermissionDecider + 'static,
{
    async fn decide(
        &self,
        request: PermissionRequest,
    ) -> Result<DeciderResponse, PermissionError> {
        let binding_id = request
            .metadata
            .get(METADATA_BINDING_ID)
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // ---- Gate 1: no binding metadata → skip relay entirely ----
        let Some(binding_id) = binding_id else {
            return self.inner.decide(request).await;
        };
        if binding_id.is_empty() {
            return self.inner.decide(request).await;
        }

        // ---- Gate 2: no eligible servers → skip relay entirely ----
        let servers = eligible_relay_servers(&self.registry, &binding_id).await;
        if servers.is_empty() {
            return self.inner.decide(request).await;
        }

        // ---- Build a stable request_id from tool_use_id ----
        let request_id = short_request_id(&request.tool_use_id);
        let params = build_request_params(&request, &request_id, &self.description_template);

        // ---- Register pending BEFORE emitting so a fast server
        // response can never arrive before we listen ----
        let receiver = match self.pending_map.register(request_id.clone()).await {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!(
                    request_id = %request_id,
                    error = %e,
                    "channel relay: pending register failed; falling back to inner decider"
                );
                return self.inner.decide(request).await;
            }
        };

        // ---- Emit one request per eligible server. Errors are
        // logged but never block — a flaky server should not
        // tank the local prompt's path. ----
        for server in &servers {
            if let Err(e) = self.dispatcher.emit_request(server, &params).await {
                tracing::warn!(
                    server = %server,
                    request_id = %request_id,
                    error = %e,
                    "channel relay: emit_request failed"
                );
            }
        }

        // ---- Race the inner decider against the channel
        // response. First to resolve wins; the loser gets
        // cancelled by Drop semantics. ----
        let request_for_inner = request.clone();
        let inner_fut = self.inner.decide(request_for_inner);
        let request_id_for_audit = request_id.clone();
        let pending_for_cancel = self.pending_map.clone();

        let outcome = tokio::select! {
            biased;
            local = inner_fut => {
                // Local prompt won. Release the channel side so a
                // late server reply doesn't double-fire.
                pending_for_cancel.cancel(&request_id_for_audit).await;
                tracing::info!(
                    request_id = %request_id_for_audit,
                    "channel relay: local decider won"
                );
                return local;
            }
            ch = receiver => {
                match ch {
                    Ok(response) => {
                        tracing::info!(
                            request_id = %request_id_for_audit,
                            from_server = %response.from_server,
                            behavior = response.behavior.as_str(),
                            "channel relay: channel response won"
                        );
                        outcome_from_channel(&response)
                    }
                    Err(_) => {
                        // Sender side dropped — should only happen
                        // when the pending entry was cancelled
                        // (e.g. shutdown). Fall back to inner.
                        tracing::warn!(
                            request_id = %request_id_for_audit,
                            "channel relay: pending channel closed without response; running inner decider"
                        );
                        return self.inner.decide(request).await;
                    }
                }
            }
        };

        Ok(DeciderResponse {
            tool_use_id: request.tool_use_id,
            outcome,
            rationale: format!(
                "channel relay: claimed by remote approver (request_id={})",
                request_id
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decider::AllowAllDecider;
    use nexo_mcp::channel::{ChannelRegistry, RegisteredChannel};
    use nexo_mcp::channel_permission::{
        DispatchError, PermissionBehavior, PermissionRelayDispatcher,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Recording dispatcher — counts emits per server, records the
    /// last params payload. Pure async, no IO.
    #[derive(Default)]
    struct RecordingDispatcher {
        emits: Mutex<Vec<(String, PermissionRequestParams)>>,
        fail: AtomicUsize,
    }

    impl RecordingDispatcher {
        fn count(&self) -> usize {
            self.emits.lock().unwrap().len()
        }
        fn last_server(&self) -> Option<String> {
            self.emits.lock().unwrap().last().map(|(s, _)| s.clone())
        }
    }

    #[async_trait]
    impl PermissionRelayDispatcher for RecordingDispatcher {
        async fn emit_request(
            &self,
            server_name: &str,
            params: &PermissionRequestParams,
        ) -> Result<(), DispatchError> {
            // `fail` is a remaining-failures counter — decrement
            // only when there's still budget so the AtomicUsize
            // never wraps around below zero.
            let pending_failures = self.fail.load(Ordering::SeqCst);
            if pending_failures > 0
                && self
                    .fail
                    .compare_exchange(
                        pending_failures,
                        pending_failures - 1,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
            {
                return Err(DispatchError::Client("forced".into()));
            }
            self.emits
                .lock()
                .unwrap()
                .push((server_name.to_string(), params.clone()));
            Ok(())
        }
    }

    /// Inner decider with controllable latency, used to win or
    /// lose the race deterministically.
    struct DelayedAllow {
        delay_ms: u64,
    }

    #[async_trait]
    impl PermissionDecider for DelayedAllow {
        async fn decide(
            &self,
            request: PermissionRequest,
        ) -> Result<DeciderResponse, PermissionError> {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            Ok(DeciderResponse {
                tool_use_id: request.tool_use_id,
                outcome: PermissionOutcome::AllowOnce {
                    updated_input: None,
                },
                rationale: "DelayedAllow".into(),
            })
        }
    }

    fn make_request(tool_use_id: &str, binding_id: Option<&str>) -> PermissionRequest {
        let mut metadata = serde_json::Map::new();
        if let Some(b) = binding_id {
            metadata.insert(
                METADATA_BINDING_ID.to_string(),
                serde_json::Value::String(b.to_string()),
            );
        }
        PermissionRequest {
            goal_id: nexo_driver_types::GoalId::new(),
            tool_use_id: tool_use_id.to_string(),
            tool_name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
            metadata,
        }
    }

    fn registered_with_relay(binding: &str, server: &str, relay: bool) -> RegisteredChannel {
        RegisteredChannel {
            binding_id: binding.into(),
            server_name: server.into(),
            plugin_source: None,
            outbound_tool_name: None,
            permission_relay: relay,
            registered_at_ms: 0,
        }
    }

    #[tokio::test]
    async fn skips_relay_when_no_binding_metadata() {
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let pending = Arc::new(PendingPermissionMap::new());
        let dispatcher: Arc<dyn PermissionRelayDispatcher> =
            Arc::new(RecordingDispatcher::default());
        let decider = ChannelRelayDecider::new(
            AllowAllDecider,
            registry.clone(),
            pending.clone(),
            dispatcher.clone(),
        );
        let resp = decider.decide(make_request("tu_a", None)).await.unwrap();
        assert!(matches!(
            resp.outcome,
            PermissionOutcome::AllowOnce { .. }
        ));
        assert_eq!(pending.len().await, 0, "no pending registered");
    }

    #[tokio::test]
    async fn skips_relay_when_binding_has_no_eligible_servers() {
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        // Server registered but permission_relay = false.
        registry
            .register(registered_with_relay("b", "slack", false))
            .await;
        let pending = Arc::new(PendingPermissionMap::new());
        let dispatcher: Arc<dyn PermissionRelayDispatcher> =
            Arc::new(RecordingDispatcher::default());
        let decider = ChannelRelayDecider::new(
            AllowAllDecider,
            registry.clone(),
            pending.clone(),
            dispatcher.clone(),
        );
        let resp = decider.decide(make_request("tu_b", Some("b"))).await.unwrap();
        assert!(matches!(
            resp.outcome,
            PermissionOutcome::AllowOnce { .. }
        ));
        assert_eq!(pending.len().await, 0);
    }

    #[tokio::test]
    async fn emits_one_request_per_eligible_server() {
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        registry
            .register(registered_with_relay("b", "slack", true))
            .await;
        registry
            .register(registered_with_relay("b", "telegram", true))
            .await;
        let pending = Arc::new(PendingPermissionMap::new());
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let dispatcher_dyn: Arc<dyn PermissionRelayDispatcher> = dispatcher.clone();
        let decider = ChannelRelayDecider::new(
            // Inner wins fast — relay still emits.
            DelayedAllow { delay_ms: 0 },
            registry.clone(),
            pending.clone(),
            dispatcher_dyn,
        );
        let _ = decider.decide(make_request("tu_c", Some("b"))).await.unwrap();
        // Both servers received the prompt before the inner won.
        assert_eq!(dispatcher.count(), 2);
        // Pending entry was registered then released once inner won.
        assert_eq!(pending.len().await, 0);
    }

    #[tokio::test]
    async fn channel_response_wins_when_local_is_slow() {
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        registry
            .register(registered_with_relay("b", "slack", true))
            .await;
        let pending = Arc::new(PendingPermissionMap::new());
        let pending_clone = pending.clone();
        let dispatcher: Arc<dyn PermissionRelayDispatcher> =
            Arc::new(RecordingDispatcher::default());
        let decider = ChannelRelayDecider::new(
            // Inner is slow; the channel reply should win.
            DelayedAllow { delay_ms: 500 },
            registry.clone(),
            pending.clone(),
            dispatcher,
        );

        // Race: kick off decide, then resolve via the pending map.
        let request = make_request("tu_d", Some("b"));
        let request_id = short_request_id(&request.tool_use_id);
        let decide_fut = tokio::spawn({
            let decider = std::sync::Arc::new(decider);
            let req = request.clone();
            async move { decider.decide(req).await }
        });

        // Give the relay decider time to register.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // Server emits a Deny response.
        let resolved = pending_clone
            .resolve(PermissionResponse {
                request_id,
                behavior: PermissionBehavior::Deny,
                from_server: "slack".into(),
            })
            .await
            .unwrap();
        assert!(resolved);

        let resp = decide_fut.await.unwrap().unwrap();
        match resp.outcome {
            PermissionOutcome::Deny { message } => {
                assert!(message.contains("slack"));
            }
            other => panic!("expected Deny, got {other:?}"),
        }
        assert!(resp.rationale.contains("channel relay"));
    }

    #[tokio::test]
    async fn local_wins_when_channel_is_slow() {
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        registry
            .register(registered_with_relay("b", "slack", true))
            .await;
        let pending = Arc::new(PendingPermissionMap::new());
        let dispatcher: Arc<dyn PermissionRelayDispatcher> =
            Arc::new(RecordingDispatcher::default());
        let decider = ChannelRelayDecider::new(
            DelayedAllow { delay_ms: 0 }, // wins immediately
            registry.clone(),
            pending.clone(),
            dispatcher,
        );
        let resp = decider.decide(make_request("tu_e", Some("b"))).await.unwrap();
        assert!(matches!(
            resp.outcome,
            PermissionOutcome::AllowOnce { .. }
        ));
        // Pending entry was cancelled.
        assert_eq!(pending.len().await, 0);
    }

    #[tokio::test]
    async fn dispatcher_failure_does_not_block_inner() {
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        registry
            .register(registered_with_relay("b", "slack", true))
            .await;
        let pending = Arc::new(PendingPermissionMap::new());
        // Dispatcher fails the first emit but inner still wins.
        let dispatcher_real = Arc::new(RecordingDispatcher::default());
        dispatcher_real.fail.store(1, Ordering::SeqCst);
        let dispatcher: Arc<dyn PermissionRelayDispatcher> = dispatcher_real.clone();
        let decider = ChannelRelayDecider::new(
            DelayedAllow { delay_ms: 0 },
            registry.clone(),
            pending.clone(),
            dispatcher,
        );
        let resp = decider.decide(make_request("tu_f", Some("b"))).await.unwrap();
        assert!(matches!(
            resp.outcome,
            PermissionOutcome::AllowOnce { .. }
        ));
    }

    #[test]
    fn build_request_params_truncates_input() {
        let big: String = "x".repeat(500);
        let req = PermissionRequest {
            goal_id: nexo_driver_types::GoalId::new(),
            tool_use_id: "tu_big".into(),
            tool_name: "Bash".into(),
            input: serde_json::json!({"command": big}),
            metadata: serde_json::Map::new(),
        };
        let p = build_request_params(&req, "abcde", "Approve?");
        assert_eq!(p.request_id, "abcde");
        assert_eq!(p.tool_name, "Bash");
        assert!(p.input_preview.ends_with('…'));
    }

    #[test]
    fn outcome_from_channel_renders_each_variant() {
        let allow = PermissionResponse {
            request_id: "abcde".into(),
            behavior: PermissionBehavior::Allow,
            from_server: "slack".into(),
        };
        assert!(matches!(
            outcome_from_channel(&allow),
            PermissionOutcome::AllowOnce { .. }
        ));
        let deny = PermissionResponse {
            request_id: "abcde".into(),
            behavior: PermissionBehavior::Deny,
            from_server: "telegram".into(),
        };
        match outcome_from_channel(&deny) {
            PermissionOutcome::Deny { message } => {
                assert!(message.contains("telegram"));
                assert!(message.contains("abcde"));
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }
}
