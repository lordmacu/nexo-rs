//! Audit-log row + filter shapes. The outcome enum is reused
//! verbatim from `crate::server::telemetry::Outcome` so a dashboard
//! and an audit query speak the same vocabulary.

use serde::{Deserialize, Serialize};

pub use crate::server::telemetry::Outcome as AuditOutcome;

/// One audit-log row. Mirrors the SQLite columns 1:1 (the store's
/// schema is the source of truth — see `sqlite_store.rs`).
///
/// `args_hash` is SHA-256 hex of the request `params.arguments`
/// payload, truncated to the first `args_hash_max_bytes` (default
/// 1 MiB). Raw args NEVER go into the row — only their hash + size.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRow {
    pub call_id: String,            // UUIDv4
    pub request_id: Option<String>, // X-Request-ID / correlation_id
    pub session_id: Option<String>, // Mcp-Session-Id
    pub tenant: String,             // Principal.tenant.as_str()
    pub subject: Option<String>,    // Principal.subject
    pub auth_method: String,        // AuthMethod variant as label
    pub method: String,             // "tools/call" | "tools/list" | …
    pub tool_name: Option<String>,  // Some only when method == "tools/call"
    pub args_hash: Option<String>,  // SHA-256 hex; None when redact off OR no args
    pub args_size_bytes: i64,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub outcome: AuditOutcome,
    pub error_code: Option<i32>, // JSON-RPC code on Error/Timeout/etc.
    pub error_message: Option<String>, // truncated to 512 chars
    pub result_size_bytes: Option<i64>,
    pub retry_after_ms: Option<i64>, // populated iff outcome == RateLimited
}

/// Read-side filter for `AuditLogStore::tail` / `count`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditFilter {
    pub tenant: Option<String>,
    pub tool_name: Option<String>,
    pub outcome: Option<AuditOutcome>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::telemetry::Outcome;

    fn sample_row() -> AuditRow {
        AuditRow {
            call_id: "00000000-0000-0000-0000-000000000001".into(),
            request_id: Some("req-1".into()),
            session_id: Some("sess-1".into()),
            tenant: "tenant-a".into(),
            subject: Some("subj-1".into()),
            auth_method: "static_token".into(),
            method: "tools/call".into(),
            tool_name: Some("echo".into()),
            args_hash: Some("a1b2c3".into()),
            args_size_bytes: 42,
            started_at_ms: 1_700_000_000_000,
            completed_at_ms: Some(1_700_000_000_500),
            duration_ms: Some(500),
            outcome: Outcome::Ok,
            error_code: None,
            error_message: None,
            result_size_bytes: Some(128),
            retry_after_ms: None,
        }
    }

    #[test]
    fn audit_row_round_trips_through_serde_json() {
        let row = sample_row();
        let s = serde_json::to_string(&row).expect("serialize");
        let back: AuditRow = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(row, back);
    }

    #[test]
    fn audit_filter_default_is_match_all() {
        let f = AuditFilter::default();
        assert!(f.tenant.is_none());
        assert!(f.tool_name.is_none());
        assert!(f.outcome.is_none());
        assert!(f.since_ms.is_none());
        assert!(f.until_ms.is_none());
    }

    #[test]
    fn audit_outcome_re_exported_from_telemetry() {
        // Sanity: the alias resolves to the bounded set 76.10
        // exposed. `outcome.as_label()` is reused.
        let o: AuditOutcome = Outcome::RateLimited;
        assert_eq!(o.as_label(), "rate_limited");
    }
}
