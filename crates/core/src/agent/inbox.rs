//! Phase 80.11 — agent inbox subject contract + payload shape.
//!
//! Multi-agent coordination via per-goal NATS inbox subject:
//! `agent.inbox.<goal_id>`. Sender (LLM tool `send_to_peer`) fires
//! and forgets; receiver subscribes per-goal and queues incoming
//! messages for injection at next turn start (deferred 80.11.b).
//!
//! Wire format is JSON; payload field carries `InboxMessage` so
//! standard NATS subject + body conventions apply.
//!
//! # Provider-agnostic
//!
//! Pure NATS + JSON. Works under any LLM provider — the subject
//! contract sits below the LLM round-trip.

use chrono::{DateTime, Utc};
use nexo_driver_types::GoalId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// NATS subject prefix for per-goal inboxes.
pub const INBOX_SUBJECT_PREFIX: &str = "agent.inbox";

/// Build the inbox subject for a goal: `agent.inbox.<goal_id>`.
pub fn inbox_subject(goal_id: GoalId) -> String {
    format!("{}.{}", INBOX_SUBJECT_PREFIX, goal_id.0)
}

/// Per-goal inbox message — fire-and-forget peer-to-peer
/// communication. Carries provenance fields so the receiver knows
/// who wrote and (optionally) which goal originated the message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxMessage {
    /// Sending agent's stable id (matches `AgentConfig.id`).
    pub from_agent_id: String,
    /// Sender's goal id at the time of sending. Useful for the
    /// receiver to reply via the sender's inbox subject.
    pub from_goal_id: GoalId,
    /// Receiver's agent id (the `to:` argument resolved to a
    /// concrete name). Repeated on the wire so subscribers don't
    /// have to parse the subject string.
    pub to_agent_id: String,
    /// Plain text body. Empty body is invalid (sender-side
    /// validation rejects).
    pub body: String,
    /// UTC timestamp at send time.
    pub sent_at: DateTime<Utc>,
    /// Optional correlation id. When set, a reply may carry the
    /// same value so request/response patterns work without a
    /// separate transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,
}

/// Minimum body length (sender-side guard against empty messages).
pub const MIN_BODY_CHARS: usize = 1;
/// Maximum body length sender-side. Receiver may impose its own
/// stricter cap. 64 KB is generous for an LLM-driven peer message
/// without being so large that broker fan-out chokes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_format_uses_prefix_dot_uuid() {
        let goal = GoalId(Uuid::nil());
        let s = inbox_subject(goal);
        assert!(s.starts_with("agent.inbox."));
        assert!(s.contains("00000000-0000-0000-0000-000000000000"));
    }

    #[test]
    fn message_serde_round_trip() {
        let msg = InboxMessage {
            from_agent_id: "kate".into(),
            from_goal_id: GoalId(Uuid::new_v4()),
            to_agent_id: "researcher".into(),
            body: "hello".into(),
            sent_at: Utc::now(),
            correlation_id: Some(Uuid::new_v4()),
        };
        let s = serde_json::to_string(&msg).unwrap();
        let back: InboxMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn correlation_id_omitted_when_none() {
        let msg = InboxMessage {
            from_agent_id: "kate".into(),
            from_goal_id: GoalId(Uuid::new_v4()),
            to_agent_id: "researcher".into(),
            body: "hello".into(),
            sent_at: Utc::now(),
            correlation_id: None,
        };
        let s = serde_json::to_string(&msg).unwrap();
        assert!(!s.contains("correlation_id"));
    }
}
