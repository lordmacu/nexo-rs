//! Phase 82.13 — operator processing pause + intervention.
//!
//! Persists a small state machine per `ProcessingScope`:
//! every scope is `AgentActive` until an operator pauses it,
//! at which point the store flips it to `PausedByOperator`
//! with the operator's bearer hash. Inbound dispatchers
//! consult the store before firing an agent turn (wire-up
//! deferred to 82.13.b — this step ships the read/write
//! API + tests).
//!
//! v0 only routes the `ProcessingScope::Conversation` +
//! `InterventionAction::Reply` combination end-to-end. Other
//! variants are reserved enum slots; the dispatcher returns
//! `-32601 not_implemented` so callers can explore the surface
//! without the daemon pretending to support unimplemented
//! shapes.

use async_trait::async_trait;
use nexo_tool_meta::admin::processing::{ProcessingControlState, ProcessingScope};

use crate::agent::admin_rpc::dispatcher::AdminRpcError;

/// Storage abstraction for the per-scope control state. v0
/// production impl is `nexo_setup::admin_adapters::InMemoryProcessingControlStore`
/// (DashMap, mirrors the in-memory pairing store pattern from
/// 82.10.h.b.1). A SQLite-backed durable variant is a 82.13.b
/// follow-up.
#[async_trait]
pub trait ProcessingControlStore: Send + Sync + std::fmt::Debug {
    /// Read the current state for `scope`. `AgentActive` is
    /// the implicit default — implementations that have never
    /// stored a row for `scope` MUST return
    /// `Ok(ProcessingControlState::AgentActive)` rather than
    /// `Err`. Lets `state` calls cost zero allocations on the
    /// happy path.
    async fn get(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<ProcessingControlState>;

    /// Set the state for `scope`. Returns `true` when the
    /// state actually changed (so handlers populate the
    /// `ProcessingAck.changed` flag accurately) and `false`
    /// when the call was a no-op (idempotent retry).
    async fn set(
        &self,
        scope: ProcessingScope,
        state: ProcessingControlState,
    ) -> anyhow::Result<bool>;

    /// Drop the row for `scope` so a subsequent `get` returns
    /// `AgentActive`. Equivalent to `set(scope, AgentActive)`
    /// but lets the store reclaim the slot.
    async fn clear(&self, scope: &ProcessingScope) -> anyhow::Result<bool>;
}

/// Boot-injected error variants reserved for the dispatcher
/// arms. Kept as a free function so handlers stay small + the
/// error wording is exercised in tests.
pub fn err_not_implemented(method: &str, detail: &str) -> AdminRpcError {
    AdminRpcError::MethodNotFound(format!(
        "not_implemented: {method} — {detail}"
    ))
}

/// `nexo/admin/processing/state` — read the current state for
/// any scope (no capability check beyond the dispatcher's gate).
pub async fn state(
    store: &dyn ProcessingControlStore,
    params: serde_json::Value,
) -> crate::agent::admin_rpc::dispatcher::AdminRpcResult {
    use nexo_tool_meta::admin::processing::{ProcessingStateParams, ProcessingStateResponse};

    let p: ProcessingStateParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::InvalidParams(e.to_string()),
            )
        }
    };
    match store.get(&p.scope).await {
        Ok(state) => crate::agent::admin_rpc::dispatcher::AdminRpcResult::ok(
            serde_json::to_value(ProcessingStateResponse { state })
                .unwrap_or(serde_json::Value::Null),
        ),
        Err(e) => crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
            AdminRpcError::Internal(format!("processing.state read: {e}")),
        ),
    }
}

/// `nexo/admin/processing/pause` — flip the scope to
/// `PausedByOperator`. Idempotent: pausing an already-paused
/// scope returns `changed = false`.
pub async fn pause(
    store: &dyn ProcessingControlStore,
    params: serde_json::Value,
) -> crate::agent::admin_rpc::dispatcher::AdminRpcResult {
    use nexo_tool_meta::admin::processing::{ProcessingAck, ProcessingPauseParams};

    let p: ProcessingPauseParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::InvalidParams(e.to_string()),
            )
        }
    };
    if !p.scope.is_v0_supported() {
        return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
            err_not_implemented(
                "nexo/admin/processing/pause",
                "v0 routes only Conversation scope",
            ),
        );
    }
    let new_state = ProcessingControlState::PausedByOperator {
        scope: p.scope.clone(),
        paused_at_ms: now_epoch_ms(),
        operator_token_hash: p.operator_token_hash,
        reason: p.reason,
    };
    let changed = match store.set(p.scope, new_state).await {
        Ok(c) => c,
        Err(e) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::Internal(format!("processing.pause set: {e}")),
            )
        }
    };
    crate::agent::admin_rpc::dispatcher::AdminRpcResult::ok(
        serde_json::to_value(ProcessingAck {
            changed,
            correlation_id: uuid::Uuid::new_v4(),
        })
        .unwrap_or(serde_json::Value::Null),
    )
}

/// `nexo/admin/processing/resume` — flip back to `AgentActive`.
/// Idempotent: resuming an already-active scope returns
/// `changed = false`.
pub async fn resume(
    store: &dyn ProcessingControlStore,
    params: serde_json::Value,
) -> crate::agent::admin_rpc::dispatcher::AdminRpcResult {
    use nexo_tool_meta::admin::processing::{ProcessingAck, ProcessingResumeParams};

    let p: ProcessingResumeParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::InvalidParams(e.to_string()),
            )
        }
    };
    if !p.scope.is_v0_supported() {
        return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
            err_not_implemented(
                "nexo/admin/processing/resume",
                "v0 routes only Conversation scope",
            ),
        );
    }
    let changed = match store.clear(&p.scope).await {
        Ok(c) => c,
        Err(e) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::Internal(format!("processing.resume clear: {e}")),
            )
        }
    };
    crate::agent::admin_rpc::dispatcher::AdminRpcResult::ok(
        serde_json::to_value(ProcessingAck {
            changed,
            correlation_id: uuid::Uuid::new_v4(),
        })
        .unwrap_or(serde_json::Value::Null),
    )
}

/// `nexo/admin/processing/intervention` — operator-driven
/// action inside a paused scope. v0 only accepts
/// `Reply` action on `Conversation` scope; the actual
/// reply-out / transcript-stamp wire-up lands in 82.13.b. Today
/// the handler validates inputs + returns ack so callers can
/// integration-test the surface.
pub async fn intervention(
    store: &dyn ProcessingControlStore,
    params: serde_json::Value,
) -> crate::agent::admin_rpc::dispatcher::AdminRpcResult {
    use nexo_tool_meta::admin::processing::{
        ProcessingAck, ProcessingInterventionParams,
    };

    let p: ProcessingInterventionParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::InvalidParams(e.to_string()),
            )
        }
    };
    if !p.scope.is_v0_supported() {
        return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
            err_not_implemented(
                "nexo/admin/processing/intervention",
                "v0 routes only Conversation scope",
            ),
        );
    }
    if !p.action.is_v0_supported() {
        return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
            err_not_implemented(
                "nexo/admin/processing/intervention",
                "v0 routes only Reply action",
            ),
        );
    }
    // Refuse intervention on a non-paused scope so operators
    // never accidentally double-respond. Audit log surfaces
    // the rejection.
    match store.get(&p.scope).await {
        Ok(ProcessingControlState::PausedByOperator { .. }) => {}
        Ok(_) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::CapabilityNotGranted {
                    capability: "operator_intervention".into(),
                    method: "nexo/admin/processing/intervention".into(),
                    microapp_id: format!("scope_not_paused:{}", p.scope.agent_id()),
                },
            )
        }
        Err(e) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::Internal(format!("processing.intervention read: {e}")),
            )
        }
    }
    crate::agent::admin_rpc::dispatcher::AdminRpcResult::ok(
        serde_json::to_value(ProcessingAck {
            changed: true,
            correlation_id: uuid::Uuid::new_v4(),
        })
        .unwrap_or(serde_json::Value::Null),
    )
}

fn now_epoch_ms() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use nexo_tool_meta::admin::processing::{
        InterventionAction, ProcessingControlState, ProcessingScope,
    };

    #[derive(Debug, Default)]
    struct MockStore {
        rows: Mutex<std::collections::HashMap<ProcessingScope, ProcessingControlState>>,
    }

    #[async_trait]
    impl ProcessingControlStore for MockStore {
        async fn get(
            &self,
            scope: &ProcessingScope,
        ) -> anyhow::Result<ProcessingControlState> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .get(scope)
                .cloned()
                .unwrap_or(ProcessingControlState::AgentActive))
        }
        async fn set(
            &self,
            scope: ProcessingScope,
            state: ProcessingControlState,
        ) -> anyhow::Result<bool> {
            let mut rows = self.rows.lock().unwrap();
            let prev = rows.insert(scope, state.clone());
            Ok(prev != Some(state))
        }
        async fn clear(&self, scope: &ProcessingScope) -> anyhow::Result<bool> {
            Ok(self.rows.lock().unwrap().remove(scope).is_some())
        }
    }

    fn convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.55".into(),
            mcp_channel_source: None,
        }
    }

    #[tokio::test]
    async fn pause_and_state_round_trip() {
        let store = MockStore::default();
        let pause_params = serde_json::json!({
            "scope": convo(),
            "operator_token_hash": "abcdef0123456789",
            "reason": "escalated",
        });
        let result = pause(&store, pause_params).await;
        assert!(result.result.is_some(), "pause ok");
        let ack: nexo_tool_meta::admin::processing::ProcessingAck =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(ack.changed, "first pause changes state");

        // state read returns PausedByOperator.
        let state_params = serde_json::json!({ "scope": convo() });
        let state_result = state(&store, state_params).await;
        let resp: nexo_tool_meta::admin::processing::ProcessingStateResponse =
            serde_json::from_value(state_result.result.unwrap()).unwrap();
        assert!(matches!(
            resp.state,
            ProcessingControlState::PausedByOperator { .. }
        ));
    }

    #[tokio::test]
    async fn pause_idempotent_returns_changed_false_on_second_call() {
        let store = MockStore::default();
        let pause_params = serde_json::json!({
            "scope": convo(),
            "operator_token_hash": "h",
        });
        let _ = pause(&store, pause_params.clone()).await;
        let result = pause(&store, pause_params).await;
        let ack: nexo_tool_meta::admin::processing::ProcessingAck =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!ack.changed, "second pause is a no-op");
    }

    #[tokio::test]
    async fn pause_rejects_non_v0_scope() {
        let store = MockStore::default();
        let result = pause(
            &store,
            serde_json::json!({
                "scope": ProcessingScope::Agent { agent_id: "ana".into() },
                "operator_token_hash": "h",
            }),
        )
        .await;
        match result.error.expect("error") {
            AdminRpcError::MethodNotFound(m) => {
                assert!(m.contains("not_implemented"), "got: {m}");
            }
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_clears_state_and_intervention_after_resume_is_rejected() {
        let store = MockStore::default();
        // Pause + resume → AgentActive again.
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let _ = resume(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;

        // intervention on a non-paused scope is rejected.
        let attempt = intervention(
            &store,
            serde_json::json!({
                "scope": convo(),
                "action": InterventionAction::Reply {
                    channel: "whatsapp".into(),
                    account_id: "acc".into(),
                    to: "wa.55".into(),
                    body: "hi".into(),
                    msg_kind: "text".into(),
                    attachments: vec![],
                    reply_to_msg_id: None,
                },
                "operator_token_hash": "h",
            }),
        )
        .await;
        assert!(matches!(
            attempt.error.expect("error"),
            AdminRpcError::CapabilityNotGranted { .. }
        ));
    }

    #[tokio::test]
    async fn intervention_accepts_reply_when_scope_is_paused() {
        let store = MockStore::default();
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let result = intervention(
            &store,
            serde_json::json!({
                "scope": convo(),
                "action": {
                    "kind": "reply",
                    "channel": "whatsapp",
                    "account_id": "acc",
                    "to": "wa.55",
                    "body": "hi",
                    "msg_kind": "text",
                },
                "operator_token_hash": "h",
            }),
        )
        .await;
        assert!(result.result.is_some(), "ok: {result:?}");
    }

    #[tokio::test]
    async fn intervention_rejects_non_v0_action() {
        let store = MockStore::default();
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let result = intervention(
            &store,
            serde_json::json!({
                "scope": convo(),
                "action": {
                    "kind": "skip_item",
                    "item_id": "x",
                    "reason": "y",
                },
                "operator_token_hash": "h",
            }),
        )
        .await;
        match result.error.expect("error") {
            AdminRpcError::MethodNotFound(m) => {
                assert!(m.contains("not_implemented"));
            }
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
    }
}
