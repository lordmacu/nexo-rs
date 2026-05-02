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
}
