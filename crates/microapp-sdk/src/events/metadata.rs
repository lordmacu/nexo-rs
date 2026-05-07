//! `EventMetadata` — partition + filter shape required by
//! [`super::store::EventStore`] for any event type it persists.
//!
//! The store needs four projections to index a row: a stable
//! string discriminator (`kind`), a primary partition string
//! (`agent_id` in the agent-creator firehose; any string for other
//! microapps), an optional tenant scope, and an event timestamp in
//! milliseconds. Implementing this trait on a `T: Serialize +
//! DeserializeOwned + Clone + Send + Sync + 'static` is sufficient
//! to use `EventStore<T>` with no other ceremony.
//!
//! A blanket impl for `nexo_tool_meta::admin::agent_events::AgentEventKind`
//! ships with the SDK so the agent-creator-microapp's firehose
//! reduces to a one-line wrapper. Microapps with bespoke event
//! types implement the trait themselves.

use nexo_tool_meta::admin::agent_events::AgentEventKind;

/// Projections an event must expose so [`super::store::EventStore`]
/// can index, filter, and time-sort rows.
pub trait EventMetadata {
    /// Stable string discriminator. Persisted to the `kind` column
    /// + used by [`super::store::ListFilter::kind`] for filtered
    /// reads. Convention: `snake_case`, version-stable.
    fn kind(&self) -> &str;

    /// Primary partition key — `agent_id` in the agent-creator
    /// firehose. Empty string is acceptable when the event is not
    /// scoped to a single agent (the column is indexed but not
    /// constrained to non-empty values).
    fn agent_id(&self) -> &str;

    /// Optional tenant scope. `None` becomes a SQL `NULL` in the
    /// `tenant_id` column; the [`super::store::ListFilter::tenant_id`]
    /// filter only matches non-NULL rows.
    fn tenant_id(&self) -> Option<&str> {
        None
    }

    /// Event timestamp in milliseconds since epoch. Drives the
    /// `at_ms` column, the `since_ms` filter, and the time-based
    /// retention sweep cutoff.
    fn at_ms(&self) -> u64;
}

impl EventMetadata for AgentEventKind {
    fn kind(&self) -> &str {
        match self {
            AgentEventKind::TranscriptAppended { .. } => "transcript_appended",
            AgentEventKind::PendingInboundsDropped { .. } => "pending_inbounds_dropped",
            AgentEventKind::EscalationRequested { .. } => "escalation_requested",
            AgentEventKind::EscalationResolved { .. } => "escalation_resolved",
            AgentEventKind::ProcessingStateChanged { .. } => "processing_state_changed",
            // Future variants the store doesn't recognise fall
            // through to a sentinel; callers that filter on `kind`
            // won't accidentally match them.
            _ => "unknown",
        }
    }

    fn agent_id(&self) -> &str {
        match self {
            AgentEventKind::TranscriptAppended { agent_id, .. }
            | AgentEventKind::PendingInboundsDropped { agent_id, .. }
            | AgentEventKind::EscalationRequested { agent_id, .. }
            | AgentEventKind::EscalationResolved { agent_id, .. }
            | AgentEventKind::ProcessingStateChanged { agent_id, .. } => agent_id,
            _ => "",
        }
    }

    fn tenant_id(&self) -> Option<&str> {
        match self {
            AgentEventKind::TranscriptAppended { tenant_id, .. }
            | AgentEventKind::EscalationRequested { tenant_id, .. }
            | AgentEventKind::EscalationResolved { tenant_id, .. }
            | AgentEventKind::ProcessingStateChanged { tenant_id, .. } => tenant_id.as_deref(),
            _ => None,
        }
    }

    fn at_ms(&self) -> u64 {
        match self {
            AgentEventKind::TranscriptAppended { sent_at_ms, .. } => *sent_at_ms,
            AgentEventKind::PendingInboundsDropped { at_ms, .. } => *at_ms,
            AgentEventKind::EscalationRequested {
                requested_at_ms, ..
            } => *requested_at_ms,
            AgentEventKind::EscalationResolved { resolved_at_ms, .. } => *resolved_at_ms,
            AgentEventKind::ProcessingStateChanged { at_ms, .. } => *at_ms,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::agent_events::TranscriptRole;
    use uuid::Uuid;

    #[test]
    fn agent_event_kind_metadata_round_trips() {
        let evt = AgentEventKind::TranscriptAppended {
            agent_id: "ana".into(),
            session_id: Uuid::nil(),
            seq: 0,
            role: TranscriptRole::User,
            body: "hi".into(),
            sent_at_ms: 1_700_000_000_000,
            sender_id: None,
            source_plugin: "whatsapp".into(),
            tenant_id: Some("acme".into()),
        };
        assert_eq!(evt.kind(), "transcript_appended");
        assert_eq!(evt.agent_id(), "ana");
        assert_eq!(evt.tenant_id(), Some("acme"));
        assert_eq!(evt.at_ms(), 1_700_000_000_000);
    }
}
