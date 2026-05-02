//! Phase 82.13 — operator processing pause + intervention
//! wire shapes.
//!
//! `ProcessingScope` + `InterventionAction` are discriminated
//! `#[non_exhaustive]` enums so future agent shapes (batch,
//! event-driven, image-gen, …) plug in as additive variants.
//! v0 ships only the chat-takeover combination —
//! `Conversation` scope + `Reply` action — but every other
//! variant exists as a reserved slot so the wire shape stays
//! forward-compatible.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Scope the operator is suspending (or operating on).
/// `#[non_exhaustive]` so adding new agent shapes later is
/// non-breaking. v0 only routes `Conversation` end-to-end;
/// other variants are accepted on the wire but the dispatcher
/// returns `-32601 not_implemented`.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcessingScope {
    /// One chat conversation. v0.
    Conversation {
        /// Owning agent.
        agent_id: String,
        /// Channel plugin (e.g. `whatsapp`).
        channel: String,
        /// Channel-side account id (e.g. WA business id).
        account_id: String,
        /// Counterparty id (e.g. WA jid).
        contact_id: String,
        /// Phase 80.9 — populated when the conversation
        /// arrived via an MCP channel server (e.g. `slack`).
        /// `None` for native-channel inbounds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mcp_channel_source: Option<String>,
    },
    /// One agent + one channel binding. Reserved slot.
    AgentBinding {
        /// Owning agent.
        agent_id: String,
        /// Channel plugin.
        channel: String,
        /// Channel-side account id.
        account_id: String,
    },
    /// Whole agent. Reserved slot.
    Agent {
        /// Agent id.
        agent_id: String,
    },
    /// NATS subject pattern. Reserved slot.
    EventStream {
        /// Owning agent.
        agent_id: String,
        /// Subject glob.
        subject_pattern: String,
    },
    /// Batch queue. Reserved slot.
    BatchQueue {
        /// Owning agent.
        agent_id: String,
        /// Queue name.
        queue_name: String,
    },
    /// Forward-compat extension hook. Reserved slot.
    Custom {
        /// Owning agent.
        agent_id: String,
        /// Caller-defined scope discriminator.
        scope_kind: String,
        /// Caller-defined scope id.
        scope_id: String,
    },
}

impl ProcessingScope {
    /// `true` when v0 routes this scope through the inbound
    /// dispatcher hook (deferred to 82.13.b). Today only
    /// `Conversation` returns true; reserved slots return
    /// false so callers can short-circuit with
    /// `not_implemented` without exposing the variant matrix.
    pub fn is_v0_supported(&self) -> bool {
        matches!(self, ProcessingScope::Conversation { .. })
    }

    /// Owning agent id — every variant has one.
    pub fn agent_id(&self) -> &str {
        match self {
            ProcessingScope::Conversation { agent_id, .. }
            | ProcessingScope::AgentBinding { agent_id, .. }
            | ProcessingScope::Agent { agent_id }
            | ProcessingScope::EventStream { agent_id, .. }
            | ProcessingScope::BatchQueue { agent_id, .. }
            | ProcessingScope::Custom { agent_id, .. } => agent_id,
        }
    }
}

/// What the operator is doing inside the paused scope.
/// Same `#[non_exhaustive]` discipline as `ProcessingScope`.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InterventionAction {
    /// Send a chat reply to the contact. v0.
    Reply {
        /// Channel plugin.
        channel: String,
        /// Channel-side account id (sender).
        account_id: String,
        /// Recipient (contact id).
        to: String,
        /// Body text or template payload.
        body: String,
        /// `text` / `template` / `media`.
        msg_kind: String,
        /// Optional attachments.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Value>,
        /// Reply-to message id, when the channel supports
        /// threaded replies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_msg_id: Option<String>,
    },
    /// Skip a queued item. Reserved slot.
    SkipItem {
        /// Item id (e.g. job id).
        item_id: String,
        /// Operator-supplied reason.
        reason: String,
    },
    /// Override an agent output. Reserved slot.
    OverrideOutput {
        /// New output payload.
        value: Value,
    },
    /// Inject a synthetic input. Reserved slot.
    InjectInput {
        /// Synthetic content.
        content: Value,
    },
    /// Forward-compat extension hook. Reserved slot. The
    /// caller-defined sub-discriminator goes in `action_kind`
    /// (NOT `kind`, which is the outer enum tag).
    Custom {
        /// Caller-defined sub-discriminator.
        action_kind: String,
        /// Caller-defined payload.
        payload: Value,
    },
}

impl InterventionAction {
    /// `true` when v0 routes this action end-to-end. Reserved
    /// slots return false; the dispatcher emits
    /// `-32601 not_implemented`.
    pub fn is_v0_supported(&self) -> bool {
        matches!(self, InterventionAction::Reply { .. })
    }
}

/// Persisted control state for one scope. `AgentActive` is the
/// default — every scope starts in this state and the store
/// only allocates a row once an operator pauses it.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProcessingControlState {
    /// Default — agent owns the scope.
    AgentActive,
    /// Operator suspended this scope. Agent skips inbounds /
    /// jobs / events that fall under it until
    /// [`InterventionAction`] (or resume) advances the state.
    PausedByOperator {
        /// Echo of the scope so callers reading state alone
        /// have the discriminator handy.
        scope: ProcessingScope,
        /// Epoch ms when the pause was set.
        paused_at_ms: u64,
        /// `token_hash` of the operator's bearer (Phase 82.12
        /// helper) so audits can correlate without storing the
        /// cleartext token.
        operator_token_hash: String,
        /// Free-form reason. Optional.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Params for `nexo/admin/processing/pause`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessingPauseParams {
    /// What to pause.
    pub scope: ProcessingScope,
    /// Free-form reason to log alongside the audit row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Operator bearer hash (matches Phase 82.12
    /// `token_hash` shape — sha256-hex truncated to 16 chars).
    /// Daemon stamps this onto the persisted state for audit
    /// correlation.
    pub operator_token_hash: String,
}

/// Response for `nexo/admin/processing/{pause, resume, intervention}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProcessingAck {
    /// Idempotency hint: `false` when the call was a no-op
    /// (e.g. pausing an already-paused scope); `true` when it
    /// actually changed state.
    pub changed: bool,
    /// `correlation_id` for log / audit lookups.
    pub correlation_id: Uuid,
    /// Phase 82.13.b.1 — `Some(true)` when the daemon appended
    /// the operator reply (or summary, or replayed inbound) to
    /// the agent transcript; `Some(false)` when the call
    /// provided no `session_id`, no transcript appender was
    /// wired, or persistence failed; `None` for calls where
    /// transcript stamping is not applicable (pause, or
    /// intervention with a non-Reply action).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_stamped: Option<bool>,
    /// Phase 82.13.b.3 — populated only on resume. Reports how
    /// many inbounds were drained from the pending queue (those
    /// captured during the pause). `Some(0)` when the queue was
    /// empty; `Some(N)` when N inbounds were stamped as
    /// synthetic User entries on the transcript; `None` for
    /// non-resume calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drained_pending: Option<u32>,
}

/// Params for `nexo/admin/processing/resume`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessingResumeParams {
    /// What to resume.
    pub scope: ProcessingScope,
    /// Operator bearer hash.
    pub operator_token_hash: String,
    /// Phase 82.13.b.2 — session in which to inject the
    /// optional summary. MUST be set whenever
    /// `summary_for_agent` is `Some`; daemon returns
    /// `-32602 invalid_params session_id_required_with_summary`
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    /// Phase 82.13.b.2 — optional operator-supplied free text
    /// the agent sees as a `System` entry on its next turn.
    /// Daemon prefixes with `[operator_summary] ` server-side,
    /// runs through the redactor, and persists alongside the
    /// regular transcript.
    ///
    /// Validation:
    /// - empty / whitespace-only after trim → `-32602 empty_summary`.
    /// - len > 4096 chars → `-32602 summary_too_long`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_for_agent: Option<String>,
}

/// Phase 82.13.b.2 — operator summary length cap. Mirrors the
/// FTS5 doc cap in `TranscriptsIndex` so a stamped summary
/// always indexes cleanly.
pub const PROCESSING_SUMMARY_MAX_LEN: usize = 4096;

/// Phase 82.13.b.3 — default per-scope inbound buffer cap.
/// Inbounds arriving while a scope is `PausedByOperator` are
/// buffered server-side instead of dropped; on resume they are
/// stamped onto the transcript as `User` entries so the agent
/// sees what the customer said during the takeover. The cap
/// bounds memory: when exceeded, the oldest entry is dropped
/// and a `PendingInboundsDropped` firehose event is emitted so
/// operators can surface the drop in the UI. v0 in-memory store
/// uses this as the only cap; durable SQLite store (82.13.c)
/// can re-tune.
pub const DEFAULT_PENDING_INBOUNDS_CAP: usize = 50;

/// Phase 82.13.b.3 — one inbound captured during a pause.
/// Persisted on the `ProcessingControlStore` keyed by scope;
/// drained back on resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingInbound {
    /// Channel-side message id when the plugin produced one.
    /// Threaded through to the resulting `User` transcript
    /// entry so audit + reply-to correlation continues to
    /// work after replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Uuid>,
    /// Counterparty id (e.g. WA jid). Lands as the `User`
    /// entry's `sender_id` on replay.
    pub from_contact_id: String,
    /// Already-redacted message body (the redactor runs on
    /// the inbound dispatcher path before the entry hits the
    /// store, mirroring the live transcript flow).
    pub body: String,
    /// Epoch ms when the inbound originally arrived. Preserved
    /// (not `now_ms()` at replay) so the agent reads the real
    /// chronology.
    pub timestamp_ms: u64,
    /// Channel/plugin that produced the inbound. Lands as the
    /// `User` entry's `source_plugin`.
    pub source_plugin: String,
}

/// Params for `nexo/admin/processing/intervention`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessingInterventionParams {
    /// What scope this intervention belongs to.
    pub scope: ProcessingScope,
    /// What the operator is doing.
    pub action: InterventionAction,
    /// Operator bearer hash.
    pub operator_token_hash: String,
    /// Phase 82.13.b.1 — session in which to stamp the operator
    /// reply on the agent transcript. When set together with a
    /// `Reply` action, the daemon appends a synthetic entry
    /// (`role: Assistant`, `source_plugin:
    /// "intervention:<channel>"`, `sender_id:
    /// "operator:<token_hash>"`) AFTER the channel-side send
    /// acks. When absent the reply still goes out but the
    /// transcript is not modified — `ProcessingAck.
    /// transcript_stamped` reports `Some(false)` so the operator
    /// UI can surface a hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
}

/// Params for `nexo/admin/processing/state`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessingStateParams {
    /// Scope to query.
    pub scope: ProcessingScope,
}

/// Response for `nexo/admin/processing/state`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessingStateResponse {
    /// Resolved control state.
    pub state: ProcessingControlState,
}

/// Notification literal emitted on every state transition. v0
/// rides on the `agent_event` firehose deferred wire-up —
/// this constant pins the method string today so the future
/// emit site is one-line.
pub const PROCESSING_STATE_CHANGED_NOTIFY_METHOD: &str =
    "nexo/notify/processing_state_changed";

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation_scope() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "55-1234".into(),
            contact_id: "55-5678".into(),
            mcp_channel_source: None,
        }
    }

    #[test]
    fn conversation_scope_round_trip_omits_unset_mcp_source() {
        let s = conversation_scope();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["kind"], "conversation");
        assert!(v.get("mcp_channel_source").is_none());
        let back: ProcessingScope = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn reply_action_round_trip_with_attachments_and_reply_to() {
        let action = InterventionAction::Reply {
            channel: "whatsapp".into(),
            account_id: "55-1234".into(),
            to: "55-5678".into(),
            body: "ya te resuelvo".into(),
            msg_kind: "text".into(),
            attachments: vec![serde_json::json!({"url": "https://x"})],
            reply_to_msg_id: Some("WAID:abc".into()),
        };
        let v = serde_json::to_value(&action).unwrap();
        assert_eq!(v["kind"], "reply");
        let back: InterventionAction = serde_json::from_value(v).unwrap();
        assert_eq!(back, action);
    }

    #[test]
    fn v0_supported_predicates_match_spec() {
        assert!(conversation_scope().is_v0_supported());
        assert!(!ProcessingScope::Agent {
            agent_id: "ana".into()
        }
        .is_v0_supported());

        assert!(matches!(
            InterventionAction::Reply {
                channel: "whatsapp".into(),
                account_id: "a".into(),
                to: "t".into(),
                body: "b".into(),
                msg_kind: "text".into(),
                attachments: vec![],
                reply_to_msg_id: None,
            },
            ref a if a.is_v0_supported()
        ));
        let skip = InterventionAction::SkipItem {
            item_id: "x".into(),
            reason: "y".into(),
        };
        assert!(!skip.is_v0_supported());
    }

    #[test]
    fn paused_state_round_trip_carries_token_hash() {
        let st = ProcessingControlState::PausedByOperator {
            scope: conversation_scope(),
            paused_at_ms: 1_700_000_000_000,
            operator_token_hash: "abcdef0123456789".into(),
            reason: Some("escalated".into()),
        };
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v["state"], "paused_by_operator");
        assert_eq!(v["operator_token_hash"], "abcdef0123456789");
        let back: ProcessingControlState = serde_json::from_value(v).unwrap();
        assert_eq!(back, st);

        // AgentActive serialises to just `{"state":"agent_active"}`.
        let active = ProcessingControlState::AgentActive;
        let av = serde_json::to_value(&active).unwrap();
        assert_eq!(av["state"], "agent_active");
        // Notification method literal pinned for cross-crate
        // consistency.
        assert_eq!(
            PROCESSING_STATE_CHANGED_NOTIFY_METHOD,
            "nexo/notify/processing_state_changed"
        );
    }

    #[test]
    fn intervention_params_round_trip_with_session_id() {
        let p = ProcessingInterventionParams {
            scope: conversation_scope(),
            action: InterventionAction::Reply {
                channel: "whatsapp".into(),
                account_id: "55-1234".into(),
                to: "55-5678".into(),
                body: "ok".into(),
                msg_kind: "text".into(),
                attachments: vec![],
                reply_to_msg_id: None,
            },
            operator_token_hash: "abcdef0123456789".into(),
            session_id: Some(Uuid::nil()),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["session_id"], "00000000-0000-0000-0000-000000000000");
        let back: ProcessingInterventionParams = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn intervention_params_legacy_payload_without_session_id_deserializes() {
        // Pre-Phase 82.13.b microapps emit no `session_id` field.
        // Wire shape MUST keep deserialising those payloads to
        // `session_id: None` so existing operator UIs keep
        // working unchanged.
        let raw = serde_json::json!({
            "scope": {
                "kind": "conversation",
                "agent_id": "ana",
                "channel": "whatsapp",
                "account_id": "55-1234",
                "contact_id": "55-5678",
            },
            "action": {
                "kind": "reply",
                "channel": "whatsapp",
                "account_id": "55-1234",
                "to": "55-5678",
                "body": "ok",
                "msg_kind": "text",
            },
            "operator_token_hash": "abcdef0123456789",
        });
        let p: ProcessingInterventionParams = serde_json::from_value(raw).unwrap();
        assert!(p.session_id.is_none());
        // And serialising the result back skips the field on the wire.
        let s = serde_json::to_string(&p).unwrap();
        assert!(!s.contains("session_id"));
    }

    #[test]
    fn resume_params_round_trip_with_session_and_summary() {
        let p = ProcessingResumeParams {
            scope: conversation_scope(),
            operator_token_hash: "h".into(),
            session_id: Some(Uuid::nil()),
            summary_for_agent: Some("cliente confirmó dirección".into()),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["session_id"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(v["summary_for_agent"], "cliente confirmó dirección");
        let back: ProcessingResumeParams = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn resume_params_legacy_payload_without_new_fields_deserializes() {
        // Pre-Phase 82.13.b.2 microapps emit no `session_id` or
        // `summary_for_agent` fields. Wire shape MUST keep
        // deserialising those payloads to None for both fields.
        let raw = serde_json::json!({
            "scope": {
                "kind": "conversation",
                "agent_id": "ana",
                "channel": "whatsapp",
                "account_id": "55-1234",
                "contact_id": "55-5678",
            },
            "operator_token_hash": "h",
        });
        let p: ProcessingResumeParams = serde_json::from_value(raw).unwrap();
        assert!(p.session_id.is_none());
        assert!(p.summary_for_agent.is_none());
        // Round-trip back skips both fields.
        let s = serde_json::to_string(&p).unwrap();
        assert!(!s.contains("session_id"));
        assert!(!s.contains("summary_for_agent"));
    }

    #[test]
    fn pending_inbound_round_trip_omits_unset_message_id() {
        let p = PendingInbound {
            message_id: None,
            from_contact_id: "wa.55".into(),
            body: "hola".into(),
            timestamp_ms: 1_700_000_000_000,
            source_plugin: "whatsapp".into(),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(!s.contains("message_id"));
        let back: PendingInbound = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn ack_drained_pending_round_trip_with_value_and_absent() {
        let with = ProcessingAck {
            changed: true,
            correlation_id: Uuid::nil(),
            transcript_stamped: None,
            drained_pending: Some(7),
        };
        let s = serde_json::to_string(&with).unwrap();
        assert!(s.contains("\"drained_pending\":7"));
        let back: ProcessingAck = serde_json::from_str(&s).unwrap();
        assert_eq!(back, with);

        let without = ProcessingAck {
            changed: false,
            correlation_id: Uuid::nil(),
            transcript_stamped: None,
            drained_pending: None,
        };
        let s = serde_json::to_string(&without).unwrap();
        assert!(!s.contains("drained_pending"));
    }

    #[test]
    fn ack_round_trip_with_transcript_stamped_present_and_absent() {
        let with = ProcessingAck {
            changed: true,
            correlation_id: Uuid::nil(),
            transcript_stamped: Some(true),
            drained_pending: None,
        };
        let s = serde_json::to_string(&with).unwrap();
        assert!(s.contains("transcript_stamped"));
        let back: ProcessingAck = serde_json::from_str(&s).unwrap();
        assert_eq!(back, with);

        let without = ProcessingAck {
            changed: false,
            correlation_id: Uuid::nil(),
            transcript_stamped: None,
            drained_pending: None,
        };
        let s = serde_json::to_string(&without).unwrap();
        assert!(!s.contains("transcript_stamped"));
        let back: ProcessingAck = serde_json::from_str(&s).unwrap();
        assert_eq!(back, without);
    }
}
