//! Phase 82.14 — agent escalation request wire shapes.
//!
//! Cross-app primitive: agents that hit work they cannot
//! complete autonomously raise an escalation against the
//! current `ProcessingScope`. Operators see a list, dismiss or
//! take over via [`EscalationsResolveParams`]. v0 ships the
//! wire types + admin RPC params; the actual `escalate_to_human`
//! built-in tool that produces these states is deferred to
//! 82.14.b alongside the BindingContext→scope derivation.
//!
//! Same `#[non_exhaustive]` discipline as 82.13's
//! `ProcessingScope` so future kinds (batch / image-gen /
//! event) plug in without breaking microapps.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::processing::ProcessingScope;

/// Operator-facing escalation reason. Closed enum (no
/// `Other` slot — agents that can't classify use `Other`,
/// which is itself a value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    /// Outside the agent's mandate.
    OutOfScope,
    /// Insufficient context to act.
    MissingData,
    /// Requires human judgement (legal, ethical, etc.).
    NeedsHumanJudgment,
    /// Customer complaint or dissatisfaction signal.
    Complaint,
    /// Agent error / inability to proceed.
    Error,
    /// Ambiguous request the agent can't disambiguate.
    Ambiguity,
    /// Policy-flagged content needs moderator review.
    PolicyViolation,
    /// Agent could not answer because the request falls outside
    /// every loaded skill / knowledge entry. Surface this from
    /// agent logic when the response would otherwise be a
    /// `"I don't know"` — the operator UI displays an
    /// "agent doesn't know" notification so a human can take
    /// over or extend the knowledge base.
    UnknownQuery,
    /// Catch-all when none of the above fits.
    Other,
}

/// Operator-facing urgency hint. Default `Normal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationUrgency {
    /// Can wait — log + notify async.
    Low,
    /// Standard SLA.
    Normal,
    /// Surface aggressively in operator UI.
    High,
}

impl Default for EscalationUrgency {
    fn default() -> Self {
        Self::Normal
    }
}

/// How a previously-pending escalation was settled.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolvedBy {
    /// Operator paused the scope via 82.13 — auto-resolved.
    OperatorTakeover,
    /// Operator dismissed without action.
    OperatorDismissed {
        /// Free-form reason — kept for the audit log.
        reason: String,
    },
    /// Agent later resolved itself (rare).
    AgentResolved,
}

/// Per-scope escalation state. `#[non_exhaustive]` so a
/// future durable variant (e.g. `Snoozed { until }`) lands
/// non-breaking.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EscalationState {
    /// Nothing pending for this scope.
    None,
    /// Agent flagged this scope; awaiting operator action.
    Pending {
        /// Scope of the work item.
        scope: ProcessingScope,
        /// Free-form prose, capped to 500 chars on the wire.
        summary: String,
        /// Closed-enum reason.
        reason: EscalationReason,
        /// Operator hint.
        urgency: EscalationUrgency,
        /// Free-form context map. Per-shape contents:
        /// - chat: `{"question": "…", "customer_phone": "…"}`
        /// - batch: `{"job_id": "…", "invalid_rows": 47}`
        /// - image-gen: `{"prompt": "…", "policy": "nudity"}`
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        context: BTreeMap<String, Value>,
        /// Epoch ms when the agent raised this.
        requested_at_ms: u64,
    },
    /// Already settled. Operator UIs render with strikethrough
    /// + the resolution metadata; new escalations on the same
    /// scope replace the resolved row.
    Resolved {
        /// Echo of the scope so callers reading state alone
        /// have the discriminator handy.
        scope: ProcessingScope,
        /// Epoch ms when settled.
        resolved_at_ms: u64,
        /// How it was settled.
        by: ResolvedBy,
    },
}

/// Filter for `nexo/admin/escalations/list`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationsListFilter {
    /// All non-resolved entries.
    #[default]
    Pending,
    /// Resolved entries only — useful for audit dashboards.
    Resolved,
    /// Both — caller dedupes by scope.
    All,
}

/// Params for `nexo/admin/escalations/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EscalationsListParams {
    /// Default `Pending` so a UI with no flags shows what
    /// needs attention.
    #[serde(default)]
    pub filter: EscalationsListFilter,
    /// Restrict to one agent. `None` returns every agent's
    /// escalations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Discriminator filter — `conversation`, `batch_queue`,
    /// `agent`, etc. Matches `ProcessingScope`'s tag value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_kind: Option<String>,
    /// Result cap. Server clamps to `[1, 1000]`. `0` →
    /// default 100.
    #[serde(default)]
    pub limit: usize,
    /// Phase 83.8.12 — multi-tenant filter. `Some(id)` returns
    /// only escalations whose owning agent has
    /// `agents.yaml.<agent_id>.tenant_id` matching. Defense-in-
    /// depth: cross-tenant queries return empty list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// One row of `EscalationsListResponse`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EscalationEntry {
    /// Owning agent.
    pub agent_id: String,
    /// Scope.
    pub scope: ProcessingScope,
    /// Current state.
    pub state: EscalationState,
}

/// Response for `nexo/admin/escalations/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EscalationsListResponse {
    /// Matching escalations newest-first (per
    /// `requested_at_ms` for `Pending` / `resolved_at_ms` for
    /// `Resolved`).
    pub entries: Vec<EscalationEntry>,
}

/// Params for `nexo/admin/escalations/resolve`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EscalationsResolveParams {
    /// Scope to resolve. Must currently be `Pending`.
    pub scope: ProcessingScope,
    /// `dismissed` (with `dismiss_reason`) or `takeover`
    /// (operator will pause via 82.13 separately).
    pub by: String,
    /// Free-form reason — required when `by = "dismissed"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismiss_reason: Option<String>,
    /// Operator bearer hash (Phase 82.12 token_hash shape).
    pub operator_token_hash: String,
}

/// Response for `nexo/admin/escalations/resolve`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EscalationsResolveResponse {
    /// `false` when the call was a no-op (no pending row).
    pub changed: bool,
    /// `correlation_id` for log lookup.
    pub correlation_id: Uuid,
}

/// Notification literal pinned for the agent_event firehose
/// extension (deferred to 82.14.b — emit site lands with the
/// inbound dispatcher hook).
pub const ESCALATION_REQUESTED_NOTIFY_KIND: &str = "escalation_requested";
/// Sibling literal for resolution events.
pub const ESCALATION_RESOLVED_NOTIFY_KIND: &str = "escalation_resolved";

/// JSON-RPC method literal for `escalations/list`.
pub const ESCALATIONS_LIST_METHOD: &str = "nexo/admin/escalations/list";
/// JSON-RPC method literal for `escalations/resolve`.
pub const ESCALATIONS_RESOLVE_METHOD: &str = "nexo/admin/escalations/resolve";

#[cfg(test)]
mod tests {
    use super::*;

    fn convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.55".into(),
            mcp_channel_source: None,
        }
    }

    #[test]
    fn pending_state_round_trip() {
        let mut ctx: BTreeMap<String, Value> = BTreeMap::new();
        ctx.insert("question".into(), Value::String("can I refund?".into()));
        let s = EscalationState::Pending {
            scope: convo(),
            summary: "customer wants a refund I cannot authorise".into(),
            reason: EscalationReason::OutOfScope,
            urgency: EscalationUrgency::High,
            context: ctx.clone(),
            requested_at_ms: 1_700_000_000_000,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["state"], "pending");
        assert_eq!(v["reason"], "out_of_scope");
        assert_eq!(v["urgency"], "high");
        let back: EscalationState = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn resolved_state_carries_kind_discriminator() {
        let s = EscalationState::Resolved {
            scope: convo(),
            resolved_at_ms: 1_700_000_001_000,
            by: ResolvedBy::OperatorDismissed {
                reason: "duplicate".into(),
            },
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["state"], "resolved");
        assert_eq!(v["by"]["kind"], "operator_dismissed");
        assert_eq!(v["by"]["reason"], "duplicate");
    }

    #[test]
    fn unknown_query_reason_round_trips_as_snake_case() {
        let v = serde_json::to_value(EscalationReason::UnknownQuery).unwrap();
        assert_eq!(v, serde_json::json!("unknown_query"));
        let back: EscalationReason = serde_json::from_value(v).unwrap();
        assert_eq!(back, EscalationReason::UnknownQuery);
    }

    #[test]
    fn list_filter_defaults_to_pending() {
        let f = EscalationsListFilter::default();
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v, serde_json::json!("pending"));
    }

    #[test]
    fn list_params_round_trip_omits_unset() {
        let p = EscalationsListParams {
            filter: EscalationsListFilter::All,
            ..Default::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(!s.contains("agent_id"));
        assert!(!s.contains("scope_kind"));
        let back: EscalationsListParams = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
        // Notify kind literals pinned.
        assert_eq!(ESCALATION_REQUESTED_NOTIFY_KIND, "escalation_requested");
        assert_eq!(ESCALATION_RESOLVED_NOTIFY_KIND, "escalation_resolved");
    }
}
