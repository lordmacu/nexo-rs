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
use nexo_tool_meta::admin::processing::{
    PendingInbound, ProcessingControlState, ProcessingScope,
};

use crate::agent::admin_rpc::channel_outbound::{
    ChannelOutboundDispatcher, ChannelOutboundError, OutboundMessage,
};
use crate::agent::admin_rpc::dispatcher::AdminRpcError;
use crate::agent::admin_rpc::transcript_appender::{
    TranscriptAppender, TranscriptEntry, TranscriptRole,
};

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

    /// Phase 82.13.b.3 — push one inbound captured during a
    /// pause onto the per-scope queue. Returns
    /// `(new_depth, dropped_count)`:
    /// - `new_depth` is the queue length AFTER the push +
    ///   eviction.
    /// - `dropped_count` is the number of OLDEST entries the
    ///   implementation evicted to honour the cap (0 on the
    ///   no-eviction path).
    ///
    /// Implementations cap the queue per scope (default
    /// [`nexo_tool_meta::admin::processing::DEFAULT_PENDING_INBOUNDS_CAP`]).
    /// FIFO eviction. Default impl is a `not_implemented` stub
    /// so legacy stores compile without forcing every
    /// `ProcessingControlStore` impl to pick this up at once;
    /// the v0 in-memory store overrides.
    async fn push_pending(
        &self,
        _scope: &ProcessingScope,
        _inbound: PendingInbound,
    ) -> anyhow::Result<(usize, u32)> {
        Err(anyhow::anyhow!(
            "push_pending not implemented for this store"
        ))
    }

    /// Phase 82.13.b.3 — drain the queue for `scope`,
    /// returning the captured inbounds in arrival order. The
    /// queue is cleared atomically. Default impl returns the
    /// empty vec so legacy stores keep `resume()` working
    /// without forcing the override (resume just won't replay
    /// anything).
    async fn drain_pending(
        &self,
        _scope: &ProcessingScope,
    ) -> anyhow::Result<Vec<PendingInbound>> {
        Ok(Vec::new())
    }

    /// Phase 82.13.b.3 — current queue length for `scope`
    /// without draining. Used by operator UIs to surface a
    /// "N inbounds pending" badge. Default impl returns 0
    /// (legacy store has no buffer).
    async fn pending_depth(
        &self,
        _scope: &ProcessingScope,
    ) -> anyhow::Result<usize> {
        Ok(0)
    }
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
            transcript_stamped: None,
            drained_pending: None,
        })
        .unwrap_or(serde_json::Value::Null),
    )
}

/// `nexo/admin/processing/resume` — flip back to `AgentActive`.
/// Idempotent: resuming an already-active scope returns
/// `changed = false`.
///
/// Phase 82.13.b.2 — when `params.summary_for_agent` is `Some`
/// AND `params.session_id` is `Some` AND the appender is wired,
/// the daemon appends a `TranscriptEntry { role: System,
/// content: "[operator_summary] <body>", source_plugin:
/// "intervention:summary", sender_id: "operator:<hash>" }` after
/// flipping state to `AgentActive`. The agent reads the summary
/// as a system directive on its next turn — most flexible
/// awareness option (operator can synthesise context without
/// forcing a literal replay).
pub async fn resume(
    store: &dyn ProcessingControlStore,
    appender: Option<&dyn TranscriptAppender>,
    params: serde_json::Value,
) -> crate::agent::admin_rpc::dispatcher::AdminRpcResult {
    use nexo_tool_meta::admin::processing::{
        ProcessingAck, ProcessingResumeParams, PROCESSING_SUMMARY_MAX_LEN,
    };

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
    // Phase 82.13.b.2 — validate summary BEFORE clearing state
    // so the operator gets a clear error and the pause stays
    // in place. Without this guard the state flip would happen
    // and the operator would see ack=changed:true even though
    // the summary they typed was invalid.
    if let Some(summary) = &p.summary_for_agent {
        if p.session_id.is_none() {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::InvalidParams(
                    "session_id_required_with_summary".into(),
                ),
            );
        }
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::InvalidParams("empty_summary".into()),
            );
        }
        if summary.chars().count() > PROCESSING_SUMMARY_MAX_LEN {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::InvalidParams("summary_too_long".into()),
            );
        }
    }
    let changed = match store.clear(&p.scope).await {
        Ok(c) => c,
        Err(e) => {
            return crate::agent::admin_rpc::dispatcher::AdminRpcResult::err(
                AdminRpcError::Internal(format!("processing.resume clear: {e}")),
            )
        }
    };
    // Phase 82.13.b.2 — best-effort summary stamp. State has
    // already flipped to Active by this point; failure here is
    // logged but does NOT roll back the resume — the alternative
    // (operator gets stuck in a paused state because their
    // optional summary couldn't persist) is worse.
    let transcript_stamped = match (
        p.summary_for_agent.as_ref(),
        p.session_id,
        appender,
    ) {
        (Some(summary), Some(session_id), Some(app)) => {
            let entry = TranscriptEntry {
                role: TranscriptRole::System,
                content: format!("[operator_summary] {}", summary.trim()),
                source_plugin: "intervention:summary".into(),
                sender_id: Some(format!("operator:{}", p.operator_token_hash)),
                message_id: None,
            };
            match app.append(p.scope.agent_id(), session_id, entry).await {
                Ok(()) => Some(true),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        agent = %p.scope.agent_id(),
                        session_id = %session_id,
                        "operator summary stamp failed; resume already cleared",
                    );
                    Some(false)
                }
            }
        }
        // No summary, or summary present but appender / session
        // missing → not applicable. Same convention as
        // intervention(): None means the field doesn't apply,
        // Some(false) means it applied but degraded.
        (None, _, _) => None,
        _ => Some(false),
    };
    crate::agent::admin_rpc::dispatcher::AdminRpcResult::ok(
        serde_json::to_value(ProcessingAck {
            changed,
            correlation_id: uuid::Uuid::new_v4(),
            transcript_stamped,
            drained_pending: None,
        })
        .unwrap_or(serde_json::Value::Null),
    )
}

/// `nexo/admin/processing/intervention` — operator-driven action
/// inside a paused scope. v0 routes `Reply` on `Conversation`
/// end-to-end through a [`ChannelOutboundDispatcher`]: the handler
/// validates inputs, asserts the scope is paused, forwards the
/// reply to the channel-plugin adapter, and (Phase 82.13.b.1)
/// stamps the reply onto the agent transcript so the agent sees
/// it on resume.
///
/// `appender` is `Some` in production (boot wires
/// `TranscriptWriterAppender`) and `None` for daemons without
/// transcript persistence wired. When `appender` is `Some` AND
/// `params.session_id` is `Some` AND the channel send acks OK,
/// the handler appends a `TranscriptEntry { role: Assistant,
/// source_plugin: "intervention:<channel>", sender_id:
/// "operator:<token_hash>" }` to the session JSONL. The Reply
/// itself succeeds regardless — `transcript_stamped` on the ack
/// reports whether stamping happened.
pub async fn intervention(
    store: &dyn ProcessingControlStore,
    outbound: Option<&dyn ChannelOutboundDispatcher>,
    appender: Option<&dyn TranscriptAppender>,
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
    //
    // Phase 82.13.b.1 — after a successful send, optionally stamp
    // the reply on the agent transcript so the agent sees it on
    // resume. The body / channel captured before the channel
    // call drives the stamp call below.
    let (outbound_message_id, reply_body, reply_channel) = match &p.action {
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
            let omid = match disp.send(msg).await {
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
            };
            (omid, body.clone(), channel.clone())
        }
        _ => unreachable!("is_v0_supported gate above limited to Reply"),
    };

    // Phase 82.13.b.1 — best-effort transcript stamp. Channel
    // send already succeeded by this point; failure here MUST
    // NOT fail the whole RPC, only set
    // `transcript_stamped: Some(false)` so the operator UI can
    // surface a hint.
    let transcript_stamped = match (p.session_id, appender) {
        (Some(session_id), Some(app)) => {
            let entry = TranscriptEntry {
                role: TranscriptRole::Assistant,
                content: reply_body,
                source_plugin: format!("intervention:{reply_channel}"),
                sender_id: Some(format!("operator:{}", p.operator_token_hash)),
                message_id: outbound_message_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok()),
            };
            match app.append(p.scope.agent_id(), session_id, entry).await {
                Ok(()) => Some(true),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        agent = %p.scope.agent_id(),
                        session_id = %session_id,
                        "transcript stamp failed; reply already sent",
                    );
                    Some(false)
                }
            }
        }
        // Either no session_id (operator UI didn't pass it) or
        // no appender wired in boot — degrade silently.
        _ => Some(false),
    };

    crate::agent::admin_rpc::dispatcher::AdminRpcResult::ok(
        serde_json::to_value(ProcessingAck {
            changed: true,
            correlation_id: uuid::Uuid::new_v4(),
            transcript_stamped,
            drained_pending: None,
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
            None,
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
            None,
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
            None,
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

    // ──────────────────────────────────────────────────────────────
    // Phase 82.13.b.1 — transcript stamping behaviour.
    // ──────────────────────────────────────────────────────────────

    /// Recording appender used in stamping tests — captures
    /// every `(agent_id, session_id, entry)` tuple without
    /// touching disk.
    #[derive(Debug, Default)]
    struct RecordingAppender {
        captured: std::sync::Mutex<
            Vec<(String, uuid::Uuid, super::TranscriptEntry)>,
        >,
        fail: bool,
    }

    #[async_trait]
    impl super::TranscriptAppender for RecordingAppender {
        async fn append(
            &self,
            agent_id: &str,
            session_id: uuid::Uuid,
            entry: super::TranscriptEntry,
        ) -> anyhow::Result<()> {
            if self.fail {
                return Err(anyhow::anyhow!("synthetic disk full"));
            }
            self.captured
                .lock()
                .unwrap()
                .push((agent_id.into(), session_id, entry));
            Ok(())
        }
    }

    fn paused_with_session() -> uuid::Uuid {
        uuid::Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap()
    }

    #[tokio::test]
    async fn intervention_stamps_transcript_when_session_and_appender_both_set() {
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
            respond_with_id: Some("550e8400-e29b-41d4-a716-446655440000".into()),
            ..Default::default()
        };
        let appender = RecordingAppender::default();
        let session_id = paused_with_session();
        let result = intervention(
            &store,
            Some(&outbound),
            Some(&appender),
            serde_json::json!({
                "scope": convo(),
                "action": {
                    "kind": "reply",
                    "channel": "whatsapp",
                    "account_id": "acc",
                    "to": "wa.55",
                    "body": "ya te resuelvo",
                    "msg_kind": "text",
                },
                "operator_token_hash": "tokhash",
                "session_id": session_id,
            }),
        )
        .await;
        assert!(result.error.is_none(), "{result:?}");
        // Channel send happened.
        assert_eq!(outbound.sent.lock().unwrap().len(), 1);
        // Transcript stamp happened with the right shape.
        let captured = appender.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let (agent_id, sid, entry) = &captured[0];
        assert_eq!(agent_id, "ana");
        assert_eq!(*sid, session_id);
        assert!(matches!(entry.role, super::TranscriptRole::Assistant));
        assert_eq!(entry.content, "ya te resuelvo");
        assert_eq!(entry.source_plugin, "intervention:whatsapp");
        assert_eq!(entry.sender_id.as_deref(), Some("operator:tokhash"));
        assert!(entry.message_id.is_some(), "outbound_message_id threaded");
        // Ack reports stamped=true.
        let v = result.result.unwrap();
        assert_eq!(v["transcript_stamped"], true);
    }

    #[tokio::test]
    async fn intervention_skips_stamp_when_session_id_absent() {
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
        let appender = RecordingAppender::default();
        let result = intervention(
            &store,
            Some(&outbound),
            Some(&appender),
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
        // Channel send still happened.
        assert_eq!(outbound.sent.lock().unwrap().len(), 1);
        // No transcript stamp.
        assert!(appender.captured.lock().unwrap().is_empty());
        // Ack reports stamped=false.
        let v = result.result.unwrap();
        assert_eq!(v["transcript_stamped"], false);
    }

    #[tokio::test]
    async fn intervention_skips_stamp_when_appender_unwired() {
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
            None, // no appender wired in boot
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
                "session_id": paused_with_session(),
            }),
        )
        .await;
        assert_eq!(outbound.sent.lock().unwrap().len(), 1);
        let v = result.result.unwrap();
        assert_eq!(v["transcript_stamped"], false);
    }

    #[tokio::test]
    async fn intervention_degrades_when_appender_returns_err() {
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
        let appender = RecordingAppender {
            fail: true,
            ..Default::default()
        };
        let result = intervention(
            &store,
            Some(&outbound),
            Some(&appender),
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
                "session_id": paused_with_session(),
            }),
        )
        .await;
        // RPC overall succeeds — the channel send is what
        // matters operationally.
        assert!(result.error.is_none(), "{result:?}");
        assert_eq!(outbound.sent.lock().unwrap().len(), 1);
        // Stamp failure surfaces only via the ack hint.
        let v = result.result.unwrap();
        assert_eq!(v["transcript_stamped"], false);
    }

    #[tokio::test]
    async fn intervention_stamp_omits_message_id_when_outbound_returns_none() {
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
            respond_with_id: None, // plugin doesn't ack with provider id
            ..Default::default()
        };
        let appender = RecordingAppender::default();
        let result = intervention(
            &store,
            Some(&outbound),
            Some(&appender),
            serde_json::json!({
                "scope": convo(),
                "action": {
                    "kind": "reply",
                    "channel": "telegram",
                    "account_id": "tg.bot",
                    "to": "tg.55",
                    "body": "hola",
                    "msg_kind": "text",
                },
                "operator_token_hash": "h",
                "session_id": paused_with_session(),
            }),
        )
        .await;
        assert!(result.error.is_none());
        let captured = appender.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let entry = &captured[0].2;
        assert!(entry.message_id.is_none());
        // Discriminator format follows :<channel>.
        assert_eq!(entry.source_plugin, "intervention:telegram");
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
            None,
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

    // ──────────────────────────────────────────────────────────────
    // Phase 82.13.b.2 — operator summary on resume.
    // ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn resume_injects_summary_as_system_entry_with_prefix() {
        let store = MockStore::default();
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let appender = RecordingAppender::default();
        let session_id = paused_with_session();
        let result = resume(
            &store,
            Some(&appender),
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "tokhash",
                "session_id": session_id,
                "summary_for_agent": "  cliente confirmó dirección  ",
            }),
        )
        .await;
        assert!(result.error.is_none(), "{result:?}");
        let captured = appender.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let (agent_id, sid, entry) = &captured[0];
        assert_eq!(agent_id, "ana");
        assert_eq!(*sid, session_id);
        assert!(matches!(entry.role, super::TranscriptRole::System));
        // Trim happens before the prefix is added.
        assert_eq!(
            entry.content,
            "[operator_summary] cliente confirmó dirección"
        );
        assert_eq!(entry.source_plugin, "intervention:summary");
        assert_eq!(entry.sender_id.as_deref(), Some("operator:tokhash"));
        let v = result.result.unwrap();
        assert_eq!(v["transcript_stamped"], true);
        assert_eq!(v["changed"], true);
    }

    #[tokio::test]
    async fn resume_rejects_summary_without_session_id() {
        let store = MockStore::default();
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let appender = RecordingAppender::default();
        let result = resume(
            &store,
            Some(&appender),
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
                "summary_for_agent": "ok",
            }),
        )
        .await;
        match result.error.expect("error") {
            AdminRpcError::InvalidParams(m) => {
                assert!(m.contains("session_id_required_with_summary"), "got: {m}");
            }
            other => panic!("expected InvalidParams, got {other:?}"),
        }
        // Defense-in-depth: state was NOT cleared because
        // validation rejected the call BEFORE the store flip.
        assert!(matches!(
            store.get(&convo()).await,
            Ok(ProcessingControlState::PausedByOperator { .. })
        ));
        // No stamp happened either.
        assert!(appender.captured.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn resume_rejects_empty_summary() {
        let store = MockStore::default();
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let result = resume(
            &store,
            None,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
                "session_id": paused_with_session(),
                "summary_for_agent": "    \t  \n",
            }),
        )
        .await;
        match result.error.expect("error") {
            AdminRpcError::InvalidParams(m) => assert!(m.contains("empty_summary")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_rejects_summary_over_4096_chars() {
        let store = MockStore::default();
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let huge = "a".repeat(4097);
        let result = resume(
            &store,
            None,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
                "session_id": paused_with_session(),
                "summary_for_agent": huge,
            }),
        )
        .await;
        match result.error.expect("error") {
            AdminRpcError::InvalidParams(m) => assert!(m.contains("summary_too_long")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_without_summary_skips_injection_and_returns_none() {
        // Legacy path — pre-Phase 82.13.b.2 microapps that
        // never send a summary. Resume MUST work identically
        // to before; transcript_stamped MUST be None (not
        // applicable) so the operator UI doesn't render a
        // stamping indicator.
        let store = MockStore::default();
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let appender = RecordingAppender::default();
        let result = resume(
            &store,
            Some(&appender),
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        assert!(result.error.is_none());
        assert!(appender.captured.lock().unwrap().is_empty());
        let v = result.result.unwrap();
        assert!(v.get("transcript_stamped").is_none());
        assert_eq!(v["changed"], true);
    }

    #[tokio::test]
    async fn resume_logs_and_proceeds_when_appender_errs() {
        let store = MockStore::default();
        let _ = pause(
            &store,
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
            }),
        )
        .await;
        let appender = RecordingAppender {
            fail: true,
            ..Default::default()
        };
        let result = resume(
            &store,
            Some(&appender),
            serde_json::json!({
                "scope": convo(),
                "operator_token_hash": "h",
                "session_id": paused_with_session(),
                "summary_for_agent": "anything",
            }),
        )
        .await;
        // Resume itself MUST succeed even when the stamp
        // fails — the alternative (state stays paused because
        // the optional summary couldn't persist) is worse.
        assert!(result.error.is_none(), "{result:?}");
        let v = result.result.unwrap();
        assert_eq!(v["transcript_stamped"], false);
        assert_eq!(v["changed"], true);
        // Store has flipped to AgentActive.
        assert!(matches!(
            store.get(&convo()).await,
            Ok(ProcessingControlState::AgentActive)
        ));
    }
}
