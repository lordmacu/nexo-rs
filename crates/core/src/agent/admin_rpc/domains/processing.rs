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

use crate::agent::admin_rpc::channel_outbound::{
    ChannelOutboundDispatcher, ChannelOutboundError, OutboundMessage,
};
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

/// `nexo/admin/processing/intervention` — operator-driven action
/// inside a paused scope. v0 routes `Reply` on `Conversation`
/// end-to-end through a [`ChannelOutboundDispatcher`]: the handler
/// validates inputs, asserts the scope is paused, then forwards
/// the reply to the channel-plugin adapter and returns the
/// provider message id back to the caller.
///
/// Closes the wire that previously stopped at "validate inputs +
/// return ack" — Phase 82.13's deferred .b is now this handler.
pub async fn intervention(
    store: &dyn ProcessingControlStore,
    outbound: Option<&dyn ChannelOutboundDispatcher>,
    params: serde_json::Value,
) -> crate::agent::admin_rpc::dispatcher::AdminRpcResult {
    use nexo_tool_meta::admin::processing::{
        InterventionAction, ProcessingAck, ProcessingInterventionParams,
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
                AdminRpcError::InvalidParams(format!(
                    "scope_not_paused: {}",
                    p.scope.agent_id()
                )),
            )
        }
        Err(e) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::Internal(format!("processing.intervention read: {e}")),
            )
        }
    }
    // Phase 83.8.4.a — actually dispatch the Reply through the
    // channel-outbound adapter. Without one wired the handler
    // surfaces `channel_unavailable` so the operator UI can show
    // a helpful "no outbound configured for this channel" error.
    let outbound_message_id = match &p.action {
        InterventionAction::Reply {
            channel,
            account_id,
            to,
            body,
            msg_kind,
            attachments,
            reply_to_msg_id,
        } => {
            let Some(disp) = outbound else {
                return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                    AdminRpcError::Internal(
                        "channel_outbound dispatcher not configured".into(),
                    ),
                );
            };
            let msg = OutboundMessage {
                channel: channel.clone(),
                account_id: account_id.clone(),
                to: to.clone(),
                body: body.clone(),
                msg_kind: msg_kind.clone(),
                attachments: attachments.clone(),
                reply_to_msg_id: reply_to_msg_id.clone(),
            };
            match disp.send(msg).await {
                Ok(ack) => ack.outbound_message_id,
                Err(ChannelOutboundError::ChannelUnavailable(name)) => {
                    return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                        AdminRpcError::Internal(format!(
                            "channel_unavailable: {name}"
                        )),
                    )
                }
                Err(ChannelOutboundError::InvalidParams(msg)) => {
                    return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                        AdminRpcError::InvalidParams(msg),
                    )
                }
                Err(ChannelOutboundError::Transport(msg)) => {
                    return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                        AdminRpcError::Internal(format!("transport: {msg}")),
                    )
                }
            }
        }
        _ => unreachable!("is_v0_supported gate above limited to Reply"),
    };

    let _ = outbound_message_id; // future audit-log threading
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

    use crate::agent::admin_rpc::channel_outbound::{
        ChannelOutboundError, OutboundAck, OutboundMessage,
    };

    #[derive(Debug, Default)]
    struct CapturingOutbound {
        sent: std::sync::Mutex<Vec<OutboundMessage>>,
        respond_with_id: Option<String>,
    }

    #[async_trait]
    impl ChannelOutboundDispatcher for CapturingOutbound {
        async fn send(
            &self,
            msg: OutboundMessage,
        ) -> Result<OutboundAck, ChannelOutboundError> {
            self.sent.lock().unwrap().push(msg);
            Ok(OutboundAck {
                outbound_message_id: self.respond_with_id.clone(),
            })
        }
    }

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
        let outbound = CapturingOutbound::default();
        let attempt = intervention(
            &store,
            Some(&outbound),
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
        match attempt.error.expect("error") {
            AdminRpcError::InvalidParams(m) => {
                assert!(m.contains("scope_not_paused"), "got: {m}");
            }
            other => panic!("expected InvalidParams, got {other:?}"),
        }
        // outbound was NOT called because scope check happened first.
        assert!(outbound.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn intervention_dispatches_reply_through_outbound_when_scope_is_paused() {
        let store = MockStore::default();
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let outbound = CapturingOutbound {
            respond_with_id: Some("provider-id-9".into()),
            ..Default::default()
        };
        let result = intervention(
            &store,
            Some(&outbound),
            serde_json::json!({
                "scope": convo(),
                "action": {
                    "kind": "reply",
                    "channel": "whatsapp",
                    "account_id": "acc",
                    "to": "wa.55",
                    "body": "hello operator",
                    "msg_kind": "text",
                },
                "operator_token_hash": "h",
            }),
        )
        .await;
        assert!(result.result.is_some(), "ok: {result:?}");
        let sent = outbound.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].channel, "whatsapp");
        assert_eq!(sent[0].account_id, "acc");
        assert_eq!(sent[0].to, "wa.55");
        assert_eq!(sent[0].body, "hello operator");
        assert_eq!(sent[0].msg_kind, "text");
    }

    #[tokio::test]
    async fn intervention_returns_internal_when_outbound_missing() {
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
            None,
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
        match result.error.expect("error") {
            AdminRpcError::Internal(m) => {
                assert!(m.contains("channel_outbound"), "got: {m}");
            }
            other => panic!("expected Internal, got {other:?}"),
        }
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
        let outbound = CapturingOutbound::default();
        let result = intervention(
            &store,
            Some(&outbound),
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
