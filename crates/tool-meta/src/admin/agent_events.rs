//! Phase 82.11 — admin RPC wire shapes for the agent event
//! firehose + backfill surface.
//!
//! Use-case agnostic by construction (Cross-cutting #6). Chat
//! is ONE variant — `TranscriptAppended` — among N. Today only
//! that variant is emitted, but [`AgentEventKind`] is enum-typed
//! with `#[non_exhaustive]` + `#[serde(tag = "kind")]` so future
//! variants (batch jobs, image-gen output, custom kinds) are
//! non-breaking additive changes.
//!
//! Subscribe semantics: live events arrive as JSON-RPC
//! notifications with method [`AGENT_EVENT_NOTIFY_METHOD`].
//! Backfill goes through the admin RPC handlers in this module.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// JSON-RPC notification method emitted on every new agent
/// event. Frame shape:
/// `{"jsonrpc":"2.0","method":"nexo/notify/agent_event",
/// "params": <AgentEventKind>}`. No `id` field — notifications
/// don't expect a response.
pub const AGENT_EVENT_NOTIFY_METHOD: &str = "nexo/notify/agent_event";

/// Discriminated event variant. v0 only emits
/// `TranscriptAppended`. Other slots are reserved enum entries
/// surfaced when the corresponding agent kind ships.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEventKind {
    /// One transcript line appended to a chat session. Body is
    /// already-redacted at emit time (the redactor runs inside
    /// `TranscriptWriter::append_entry` before this frame is
    /// produced).
    TranscriptAppended {
        /// Owning agent.
        agent_id: String,
        /// Conversation scope.
        session_id: Uuid,
        /// Monotonic sequence within `session_id` — equal to the
        /// 0-based index of the entry in the session's JSONL.
        seq: u64,
        /// Speaker role.
        role: TranscriptRole,
        /// Redacted body (PII replaced with `[REDACTED:label]`).
        body: String,
        /// Epoch milliseconds when the entry was recorded.
        sent_at_ms: u64,
        /// Opaque external sender id when applicable (channel
        /// user id, peer agent id, …).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_id: Option<String>,
        /// Channel/plugin that produced or received the entry
        /// (e.g. `whatsapp`, `telegram`, `internal`).
        source_plugin: String,
        /// Phase 83.8.12 — owning tenant. `None` for agents
        /// predating multi-tenant. Firehose subscribers filter
        /// on this without re-querying agents.yaml.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tenant_id: Option<String>,
    },
    /// Phase 82.13.b.3 — fired when the inbound dispatcher
    /// captures an inbound during pause but the per-scope
    /// pending queue is at the cap and the oldest entry had
    /// to be evicted. Operator UIs can render a "history
    /// thinning" hint so the operator knows the agent will not
    /// see those evicted messages on resume. The `dropped`
    /// count is the number of entries evicted by THIS push (1
    /// per push under the FIFO cap policy; future variants
    /// may batch). `agent_id` is denormalised so subscribers
    /// filter without parsing `scope`.
    PendingInboundsDropped {
        /// Owning agent.
        agent_id: String,
        /// Scope under which the drop happened. Carried whole
        /// (rather than just `(channel, account_id, contact_id)`)
        /// so future scope variants stay forward-compatible.
        scope: crate::admin::processing::ProcessingScope,
        /// Number of pending inbounds evicted by THIS push.
        dropped: u32,
        /// Epoch ms when the eviction happened.
        at_ms: u64,
    },
    /// Phase 82.14.b — fired when an agent flags a scope for
    /// human review via the escalations admin RPC. Operator UIs
    /// render a badge / notification so the operator picks it up
    /// without polling `nexo/admin/escalations/list`.
    EscalationRequested {
        /// Owning agent.
        agent_id: String,
        /// Scope of the work item the agent is escalating.
        scope: crate::admin::processing::ProcessingScope,
        /// Free-form summary the agent supplied (already capped
        /// to 500 chars by the admin handler before emit).
        summary: String,
        /// Closed-enum reason for the escalation.
        reason: crate::admin::escalations::EscalationReason,
        /// Operator hint on how aggressively to surface.
        urgency: crate::admin::escalations::EscalationUrgency,
        /// Epoch ms when the agent raised the request.
        requested_at_ms: u64,
        /// Phase 83.8.12 — owning tenant; `None` for legacy /
        /// single-tenant deployments. Firehose subscribers route
        /// per-tenant without re-querying `agents.yaml`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tenant_id: Option<String>,
    },
    /// Phase 82.14.b — fired when an escalation transitions out
    /// of `Pending`. Carries the same `agent_id` + `scope` keys
    /// as the matching `EscalationRequested` so subscribers can
    /// pair the two via `(agent_id, scope)` regardless of the
    /// settle path (operator dismiss / takeover / agent resolve).
    EscalationResolved {
        /// Owning agent.
        agent_id: String,
        /// Scope that was settled. Same value the `Pending`
        /// emit carried.
        scope: crate::admin::processing::ProcessingScope,
        /// Epoch ms when the resolution happened.
        resolved_at_ms: u64,
        /// How the escalation was settled.
        by: crate::admin::escalations::ResolvedBy,
        /// Phase 83.8.12 — owning tenant. Mirrors the
        /// `Requested` shape so the pair stays
        /// shape-symmetrical.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tenant_id: Option<String>,
    },
    /// Phase 82.13.b.firehose — fired when an admin RPC handler
    /// flips the [`crate::admin::processing::ProcessingControlState`]
    /// for a scope (operator pause / resume / intervention reply).
    /// Operator UIs render a real-time pause indicator without
    /// polling `processing/state`. `prev_state` carries the value
    /// the handler observed before the transition so subscribers
    /// can render correct deltas (e.g. badge clears on
    /// `paused → active`); on initial pause the `prev` is
    /// `agent_active`. The matching JSON-RPC method literal lives
    /// at [`crate::admin::processing::PROCESSING_STATE_CHANGED_NOTIFY_METHOD`]
    /// — emit sites use the firehose `agent_event` notify method
    /// (this variant rides on the same channel as every other
    /// agent event), but the constant is kept for any future
    /// dedicated subject.
    ProcessingStateChanged {
        /// Owning agent (denormalised from `scope` so subscribers
        /// filter without parsing the discriminator).
        agent_id: String,
        /// Scope whose state flipped.
        scope: crate::admin::processing::ProcessingScope,
        /// State observed immediately before the transition.
        prev_state: crate::admin::processing::ProcessingControlState,
        /// State the handler installed.
        new_state: crate::admin::processing::ProcessingControlState,
        /// Epoch ms when the transition happened.
        at_ms: u64,
        /// Phase 83.8.12 — owning tenant. `None` for legacy /
        /// single-tenant deployments.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tenant_id: Option<String>,
    },
    /// Phase 82.10.o — security-domain audit event. Carries a
    /// nested [`SecurityEventKind`] discriminator so future
    /// security events (operator login, capability grant, …)
    /// extend the shape without proliferating top-level
    /// variants. Today only [`SecurityEventKind::TokenRotated`]
    /// fires, from the daemon's `FsAuthRotator` post-rotation.
    SecurityEvent {
        /// Discriminated security-event payload.
        #[serde(flatten)]
        event: SecurityEventKind,
    },
}

/// Phase 82.10.o — security-domain audit events. Nested under
/// [`AgentEventKind::SecurityEvent`] so subscribers persist
/// every variant alongside transcript / escalation events for
/// a unified audit log.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "security_kind", rename_all = "snake_case")]
pub enum SecurityEventKind {
    /// Operator bearer token rotated. Emitted by the daemon's
    /// `FsAuthRotator` after a successful
    /// `nexo/admin/auth/rotate_token` call. Subscribers persist
    /// for compliance audit trail.
    TokenRotated {
        /// Epoch milliseconds when the rotation persisted.
        at_ms: u64,
        /// Previous operator-token-hash (16-char sha256-hex
        /// prefix). Zeroed (`""`) for the very first rotation
        /// after boot when no prior hash is in cache.
        prev_hash: String,
        /// New operator-token-hash.
        new_hash: String,
        /// Optional operator-supplied audit hint, capped to
        /// `auth::REASON_MAX_LEN` chars by the handler.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Speaker role mirror — kept on the wire side so SDK types
/// don't pull in the core crate. Matches
/// `nexo_core::agent::transcripts::TranscriptRole`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    /// Inbound from end-user.
    User,
    /// Outbound from the agent.
    Assistant,
    /// Tool call result echoed back into the transcript.
    Tool,
    /// System-level event (e.g. system prompt seed, control message).
    System,
}

/// Params for `nexo/admin/agent_events/list`. All filters
/// optional. v0 only honours `kind = "transcript_appended"` (or
/// `None` which behaves the same).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEventsListFilter {
    /// Required — events for this agent.
    pub agent_id: String,
    /// Optional discriminator filter. v0: `transcript_appended`
    /// (or unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Lower-bound timestamp (epoch ms). Server defaults to
    /// `now - 30d` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// Result cap. Server clamps to `[1, 1000]`. `0` → server
    /// default (500).
    #[serde(default)]
    pub limit: usize,
    /// Phase 83.8.12 — multi-tenant filter. `Some(id)` requires
    /// the inbound that produced the event was bound to an
    /// agent with `agents.yaml.<agent_id>.tenant_id` matching.
    /// Defense-in-depth: cross-tenant queries return empty
    /// instead of leaking existence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// Response for `nexo/admin/agent_events/list`. Newest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEventsListResponse {
    /// Matching events in newest-first order.
    pub events: Vec<AgentEventKind>,
}

/// Params for `nexo/admin/agent_events/read`. Reads one scope
/// (`session_id` for `TranscriptAppended`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEventsReadParams {
    /// Owning agent.
    pub agent_id: String,
    /// Conversation scope. The trait surface delegates to the
    /// concrete reader based on `kind`; v0 always reads
    /// `TranscriptAppended` so this is the chat session id.
    pub session_id: Uuid,
    /// Resume after this seq (exclusive). `None` returns from
    /// the start of the scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
    /// Result cap. Server clamps to `[1, 1000]`. `0` → default
    /// 200.
    #[serde(default)]
    pub limit: usize,
}

/// Response for `nexo/admin/agent_events/read`. Oldest-first
/// within the scope so a caller streaming with `since_seq`
/// gets a contiguous tail.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEventsReadResponse {
    /// Events for the scope ordered ascending by `seq`.
    pub events: Vec<AgentEventKind>,
}

/// Params for `nexo/admin/agent_events/search`. v0 backed by
/// the existing `TranscriptsIndex` (FTS5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEventsSearchParams {
    /// Owning agent.
    pub agent_id: String,
    /// Free-text query. The handler escapes FTS5 operators so
    /// arbitrary user input is safe.
    pub query: String,
    /// Optional discriminator filter. v0: `transcript_appended`
    /// (or unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Result cap. Server clamps to `[1, 500]`. `0` → default
    /// 50.
    #[serde(default)]
    pub limit: usize,
}

/// Response for `nexo/admin/agent_events/search`. Ranked
/// best-match first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEventsSearchResponse {
    /// Hits ranked by FTS5 relevance.
    pub hits: Vec<SearchHit>,
}

/// One FTS5 hit. `snippet` carries the match-marked excerpt
/// (`[token]` highlights) so UIs render result lists without
/// re-fetching the full session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    /// Conversation scope of the hit.
    pub session_id: Uuid,
    /// Epoch milliseconds when the matching entry was recorded.
    pub timestamp_ms: u64,
    /// Speaker role.
    pub role: TranscriptRole,
    /// Channel/plugin that produced the entry.
    pub source_plugin: String,
    /// FTS5 snippet (already redacted at index time, since the
    /// indexer runs over the same redacted body the firehose
    /// emits).
    pub snippet: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_appended_round_trip_includes_optional_sender() {
        let evt = AgentEventKind::TranscriptAppended {
            agent_id: "ana".into(),
            session_id: Uuid::nil(),
            seq: 7,
            role: TranscriptRole::User,
            body: "[REDACTED:phone] hola".into(),
            sent_at_ms: 1_700_000_000_000,
            sender_id: Some("wa.55123".into()),
            source_plugin: "whatsapp".into(),
            tenant_id: None,
        };
        let v = serde_json::to_value(&evt).unwrap();
        // Discriminator is present on the wire.
        assert_eq!(v["kind"], "transcript_appended");
        assert_eq!(v["seq"], 7);
        assert_eq!(v["role"], "user");
        let back: AgentEventKind = serde_json::from_value(v).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn transcript_appended_omits_unset_sender() {
        let evt = AgentEventKind::TranscriptAppended {
            agent_id: "ana".into(),
            session_id: Uuid::nil(),
            seq: 0,
            role: TranscriptRole::Assistant,
            body: "ok".into(),
            sent_at_ms: 1,
            sender_id: None,
            source_plugin: "internal".into(),
            tenant_id: None,
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(!s.contains("sender_id"), "absent sender skipped on wire");
    }

    #[test]
    fn list_filter_round_trip_with_defaults() {
        let f = AgentEventsListFilter {
            agent_id: "ana".into(),
            kind: None,
            since_ms: None,
            limit: 0,
            tenant_id: None,
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(!s.contains("kind"));
        assert!(!s.contains("since_ms"));
        let back: AgentEventsListFilter = serde_json::from_str(&s).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn search_params_round_trip_and_notify_method_constant() {
        let p = AgentEventsSearchParams {
            agent_id: "ana".into(),
            query: "hola \"phone\"".into(),
            kind: Some("transcript_appended".into()),
            limit: 25,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["query"], "hola \"phone\"");
        let back: AgentEventsSearchParams = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);

        assert_eq!(AGENT_EVENT_NOTIFY_METHOD, "nexo/notify/agent_event");
    }

    #[test]
    fn processing_state_changed_round_trip_carries_prev_and_new() {
        use crate::admin::processing::{ProcessingControlState, ProcessingScope};

        let scope = ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "55-1234".into(),
            contact_id: "55-5678".into(),
            mcp_channel_source: None,
        };
        let evt = AgentEventKind::ProcessingStateChanged {
            agent_id: "ana".into(),
            scope: scope.clone(),
            prev_state: ProcessingControlState::AgentActive,
            new_state: ProcessingControlState::PausedByOperator {
                scope: scope.clone(),
                paused_at_ms: 1_700_000_000_000,
                operator_token_hash: "abcdef0123456789".into(),
                reason: Some("escalated".into()),
            },
            at_ms: 1_700_000_000_000,
            tenant_id: None,
        };
        let v = serde_json::to_value(&evt).unwrap();
        assert_eq!(v["kind"], "processing_state_changed");
        assert_eq!(v["agent_id"], "ana");
        assert_eq!(v["prev_state"]["state"], "agent_active");
        assert_eq!(v["new_state"]["state"], "paused_by_operator");
        assert!(
            v.get("tenant_id").is_none(),
            "tenant_id absent must be skipped"
        );
        let back: AgentEventKind = serde_json::from_value(v).unwrap();
        assert_eq!(back, evt);
    }

    /// Phase 82.10.o — token-rotation security event round-trips
    /// through the firehose envelope with all four fields
    /// (timestamp + two hashes + reason).
    #[test]
    fn security_event_token_rotated_round_trip_with_reason() {
        let evt = AgentEventKind::SecurityEvent {
            event: SecurityEventKind::TokenRotated {
                at_ms: 1_700_000_000_123,
                prev_hash: "cafebabedeadbeef".into(),
                new_hash: "1234567890abcdef".into(),
                reason: Some("scheduled rotation".into()),
            },
        };
        let v = serde_json::to_value(&evt).unwrap();
        assert_eq!(v["kind"], "security_event");
        // Nested discriminator from `#[serde(flatten)]`.
        assert_eq!(v["security_kind"], "token_rotated");
        assert_eq!(v["at_ms"], 1_700_000_000_123u64);
        assert_eq!(v["prev_hash"], "cafebabedeadbeef");
        assert_eq!(v["new_hash"], "1234567890abcdef");
        assert_eq!(v["reason"], "scheduled rotation");
        let back: AgentEventKind = serde_json::from_value(v).unwrap();
        assert_eq!(back, evt);
    }

    /// Phase 82.10.o — `reason: None` is skipped on the wire so
    /// audit firehose payloads stay tight when the operator
    /// didn't supply one.
    #[test]
    fn security_event_token_rotated_omits_unset_reason() {
        let evt = AgentEventKind::SecurityEvent {
            event: SecurityEventKind::TokenRotated {
                at_ms: 1_700_000_000_000,
                prev_hash: String::new(),
                new_hash: "deadbeefcafebabe".into(),
                reason: None,
            },
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(!s.contains("reason"), "absent reason skipped on wire");
        let back: AgentEventKind = serde_json::from_value(serde_json::from_str(&s).unwrap())
            .unwrap();
        assert_eq!(back, evt);
    }
}
