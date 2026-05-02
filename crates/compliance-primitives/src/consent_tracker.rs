//! Consent tracker.
//!
//! In-memory opt-in / opt-out store for GDPR + CAN-SPAM +
//! WhatsApp Business Policy compliance. Microapps use this to
//! gate outbound messages — no consent → no message.
//!
//! Two-state model: a user is `OptedIn`, `OptedOut`, or
//! `Unknown` (never recorded). The default state is `Unknown`,
//! which the spec says microapps must treat as "do not contact"
//! for cold outbound (CAN-SPAM "express consent" requirement).
//!
//! Timestamped audit trail per change so the operator can prove
//! when consent was given / withdrawn.
//!
//! Persistence is the microapp's responsibility — this struct
//! lives in memory; a real deployment serialises the records to
//! the per-extension state dir (Phase 82.6) on every change.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentStatus {
    OptedIn,
    OptedOut,
    /// Never recorded; default-deny for cold outbound.
    Unknown,
}

impl ConsentStatus {
    /// Convenience: `true` when outbound messaging is permitted
    /// under this status. Spec calls for `OptedIn` only —
    /// `OptedOut` is explicit refusal, `Unknown` is "no record"
    /// which under CAN-SPAM defaults to no.
    pub fn allows_outbound(self) -> bool {
        matches!(self, ConsentStatus::OptedIn)
    }
}

/// One change-event in the audit trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub user_key: String,
    pub status: ConsentStatus,
    pub at: DateTime<Utc>,
    /// Operator-supplied source of the change — e.g.
    /// `"web_form"`, `"opt_out_keyword"`, `"manual"`. Surfaces
    /// in audit dashboards.
    pub source: String,
}

/// In-memory consent store with a per-user audit trail.
#[derive(Debug, Default)]
pub struct ConsentTracker {
    current: BTreeMap<String, ConsentStatus>,
    history: Vec<ConsentRecord>,
}

impl ConsentTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an explicit opt-in. `source` is a free-form string
    /// for the audit log.
    pub fn opt_in(&mut self, user_key: &str, source: &str) {
        self.upsert(user_key, ConsentStatus::OptedIn, source, Utc::now());
    }

    /// Record an explicit opt-out.
    pub fn opt_out(&mut self, user_key: &str, source: &str) {
        self.upsert(user_key, ConsentStatus::OptedOut, source, Utc::now());
    }

    /// Test-only / time-injectable variant.
    #[doc(hidden)]
    pub fn upsert_at(
        &mut self,
        user_key: &str,
        status: ConsentStatus,
        source: &str,
        at: DateTime<Utc>,
    ) {
        self.upsert(user_key, status, source, at);
    }

    fn upsert(
        &mut self,
        user_key: &str,
        status: ConsentStatus,
        source: &str,
        at: DateTime<Utc>,
    ) {
        self.current.insert(user_key.to_string(), status);
        self.history.push(ConsentRecord {
            user_key: user_key.to_string(),
            status,
            at,
            source: source.to_string(),
        });
    }

    /// Lookup the current status. Returns `Unknown` when the
    /// user has never been recorded.
    pub fn status(&self, user_key: &str) -> ConsentStatus {
        self.current
            .get(user_key)
            .copied()
            .unwrap_or(ConsentStatus::Unknown)
    }

    /// Convenience for the gate: outbound permitted?
    pub fn allows_outbound(&self, user_key: &str) -> bool {
        self.status(user_key).allows_outbound()
    }

    /// Read-only view of the audit log, oldest first.
    pub fn history(&self) -> &[ConsentRecord] {
        &self.history
    }

    /// Filter the audit log to one user's entries.
    pub fn history_for_user<'a>(
        &'a self,
        user_key: &'a str,
    ) -> impl Iterator<Item = &'a ConsentRecord> + 'a {
        self.history.iter().filter(move |r| r.user_key == user_key)
    }

    pub fn user_count(&self) -> usize {
        self.current.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0).unwrap()
    }

    #[test]
    fn unknown_user_defaults_to_no_outbound() {
        let t = ConsentTracker::new();
        assert_eq!(t.status("alice"), ConsentStatus::Unknown);
        assert!(!t.allows_outbound("alice"));
    }

    #[test]
    fn opt_in_permits_outbound() {
        let mut t = ConsentTracker::new();
        t.opt_in("alice", "web_form");
        assert_eq!(t.status("alice"), ConsentStatus::OptedIn);
        assert!(t.allows_outbound("alice"));
    }

    #[test]
    fn opt_out_blocks_outbound() {
        let mut t = ConsentTracker::new();
        t.opt_in("alice", "web");
        t.opt_out("alice", "stop_keyword");
        assert_eq!(t.status("alice"), ConsentStatus::OptedOut);
        assert!(!t.allows_outbound("alice"));
    }

    #[test]
    fn audit_log_records_each_change() {
        let mut t = ConsentTracker::new();
        t.upsert_at("alice", ConsentStatus::OptedIn, "web", ts(2026, 4, 1));
        t.upsert_at(
            "alice",
            ConsentStatus::OptedOut,
            "stop_keyword",
            ts(2026, 4, 15),
        );
        let rows: Vec<&ConsentRecord> = t.history_for_user("alice").collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, ConsentStatus::OptedIn);
        assert_eq!(rows[0].source, "web");
        assert_eq!(rows[1].status, ConsentStatus::OptedOut);
        assert_eq!(rows[1].source, "stop_keyword");
        // History is monotonic.
        assert!(rows[0].at < rows[1].at);
    }

    #[test]
    fn audit_log_filters_per_user() {
        let mut t = ConsentTracker::new();
        t.opt_in("alice", "web");
        t.opt_in("bob", "phone");
        t.opt_out("alice", "stop");
        assert_eq!(t.history_for_user("alice").count(), 2);
        assert_eq!(t.history_for_user("bob").count(), 1);
        assert_eq!(t.history().len(), 3);
    }

    #[test]
    fn user_count_tracks_distinct_users() {
        let mut t = ConsentTracker::new();
        t.opt_in("a", "x");
        t.opt_in("b", "x");
        t.opt_in("a", "x"); // re-upsert same user
        assert_eq!(t.user_count(), 2);
    }

    #[test]
    fn consent_record_serde_round_trip() {
        let r = ConsentRecord {
            user_key: "alice".into(),
            status: ConsentStatus::OptedIn,
            at: ts(2026, 5, 1),
            source: "web_form".into(),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: ConsentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn allows_outbound_only_for_opted_in() {
        assert!(ConsentStatus::OptedIn.allows_outbound());
        assert!(!ConsentStatus::OptedOut.allows_outbound());
        assert!(!ConsentStatus::Unknown.allows_outbound());
    }
}
