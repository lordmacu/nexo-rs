//! Phase 80.14 — re-connection digest helper. Slim MVP: template-based,
//! no LLM call. The forked-LLM-summarised variant lands as 80.14.b.
//!
//! Composes a short markdown digest summarising goals + aborts +
//! failures recorded in the Phase 72 turn-log during the silence
//! window. Caller is responsible for tracking `last_seen_at` per
//! (channel, sender_id) and updating it after composing the digest
//! (atomic update keeps double-inbound from re-firing).
//!
//! # Provider-agnostic
//!
//! No LLM call in MVP. Pure template render over a `Vec<TurnRecord>`.
//! Works under any provider since the data path is filesystem +
//! SQLite only.

use std::time::Duration;

use chrono::{DateTime, Utc};
use nexo_agent_registry::{AgentRegistryStoreError, TurnLogStore, TurnRecord};
use nexo_config::types::away_summary::AwaySummaryConfig;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AwaySummaryError {
    #[error("turn-log read failed: {0}")]
    Store(#[from] AgentRegistryStoreError),
}

/// Return a markdown digest if all gates pass, else None.
///
/// Gates (cheapest first):
/// 1. `cfg.enabled` — opt-in only
/// 2. `last_seen` is `Some(_)` — `None` bootstraps without firing
/// 3. `now - last_seen >= cfg.threshold()` — silence window elapsed
/// 4. The turn-log has at least one event since `last_seen` — empty
///    digest is not worth sending
///
/// Caller is responsible for updating `last_seen_at = now` ONLY after
/// deciding to fire (so a no-fire bootstrap doesn't burn the
/// threshold). Atomic update keeps double-inbound from re-firing.
pub async fn try_compose_away_digest(
    cfg: &AwaySummaryConfig,
    last_seen: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    log: &dyn TurnLogStore,
) -> Result<Option<String>, AwaySummaryError> {
    if !cfg.enabled {
        return Ok(None);
    }
    let last = match last_seen {
        Some(t) => t,
        None => return Ok(None),
    };
    let elapsed = match (now - last).to_std() {
        Ok(d) => d,
        Err(_) => return Ok(None), // negative (clock skew)
    };
    if elapsed < cfg.threshold() {
        return Ok(None);
    }
    let records = log.tail_since(last, cfg.max_events).await?;
    if records.is_empty() {
        return Ok(None);
    }
    Ok(Some(build_digest(&records, elapsed, cfg.max_events)))
}

/// Pure-fn renderer. Public for tests + future LLM-summarised path
/// (80.14.b) that may want to fold the same counters into a richer
/// prompt body.
pub fn build_digest(events: &[TurnRecord], elapsed: Duration, max_events: usize) -> String {
    let total_secs = elapsed.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;

    let mut s = format!(
        "**While you were away** (last {}h{}m):\n",
        hours, minutes
    );

    let n_completed = events
        .iter()
        .filter(|e| e.outcome.contains("completed") || e.outcome == "done")
        .count();
    let n_aborted = events
        .iter()
        .filter(|e| e.outcome.contains("aborted") || e.outcome.contains("cancelled"))
        .count();
    let n_failed = events
        .iter()
        .filter(|e| e.outcome.contains("failed"))
        .count();
    let n_other = events
        .len()
        .saturating_sub(n_completed + n_aborted + n_failed);

    s.push_str(&format!("- {} goal turn(s) recorded\n", events.len()));
    if n_completed > 0 {
        s.push_str(&format!("- {} completed\n", n_completed));
    }
    if n_aborted > 0 {
        s.push_str(&format!("- {} aborted/cancelled\n", n_aborted));
    }
    if n_failed > 0 {
        s.push_str(&format!("- {} failed\n", n_failed));
    }
    if n_other > 0 {
        s.push_str(&format!("- {} in progress / other\n", n_other));
    }

    if events.len() == max_events {
        // Caller hit the cap — operator should know more events
        // exist beyond the window we summarised.
        s.push_str(&format!(
            "\n_(showing the most recent {} — older events may exist)_\n",
            max_events
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use nexo_driver_types::GoalId;
    use uuid::Uuid;

    /// In-memory mock that returns a scripted `Vec<TurnRecord>` from
    /// `tail_since` regardless of arguments. The other trait methods
    /// are no-ops or unimplemented for the digest tests.
    struct MockLog {
        records: Vec<TurnRecord>,
    }

    #[async_trait]
    impl TurnLogStore for MockLog {
        async fn append(&self, _r: &TurnRecord) -> Result<(), AgentRegistryStoreError> {
            Ok(())
        }
        async fn tail(
            &self,
            _g: GoalId,
            _n: usize,
        ) -> Result<Vec<TurnRecord>, AgentRegistryStoreError> {
            Ok(self.records.clone())
        }
        async fn count(&self, _g: GoalId) -> Result<u64, AgentRegistryStoreError> {
            Ok(self.records.len() as u64)
        }
        async fn drop_for_goal(&self, _g: GoalId) -> Result<u64, AgentRegistryStoreError> {
            Ok(0)
        }
        async fn tail_since(
            &self,
            _since: DateTime<Utc>,
            limit: usize,
        ) -> Result<Vec<TurnRecord>, AgentRegistryStoreError> {
            Ok(self.records.iter().take(limit).cloned().collect())
        }
    }

    fn rec(outcome: &str) -> TurnRecord {
        TurnRecord {
            goal_id: GoalId(Uuid::new_v4()),
            turn_index: 1,
            recorded_at: Utc::now(),
            outcome: outcome.into(),
            decision: None,
            summary: None,
            diff_stat: None,
            error: None,
            raw_json: "{}".into(),
            source: None,
        }
    }

    fn enabled_cfg() -> AwaySummaryConfig {
        AwaySummaryConfig {
            enabled: true,
            threshold_hours: 4,
            max_events: 50,
        }
    }

    #[tokio::test]
    async fn disabled_returns_none() {
        let cfg = AwaySummaryConfig::default(); // enabled = false
        let log = MockLog { records: vec![rec("done")] };
        let now = Utc::now();
        let last = now - chrono::Duration::hours(10);
        let r = try_compose_away_digest(&cfg, Some(last), now, &log)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn last_seen_none_returns_none() {
        let cfg = enabled_cfg();
        let log = MockLog { records: vec![rec("done")] };
        let r = try_compose_away_digest(&cfg, None, Utc::now(), &log)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn elapsed_below_threshold_returns_none() {
        let cfg = enabled_cfg(); // 4h
        let log = MockLog { records: vec![rec("done")] };
        let now = Utc::now();
        let last = now - chrono::Duration::hours(2);
        let r = try_compose_away_digest(&cfg, Some(last), now, &log)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn negative_elapsed_returns_none() {
        let cfg = enabled_cfg();
        let log = MockLog { records: vec![rec("done")] };
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        // last_seen is in the future relative to now — clock skew.
        let last = now + chrono::Duration::hours(1);
        let r = try_compose_away_digest(&cfg, Some(last), now, &log)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn empty_log_returns_none() {
        let cfg = enabled_cfg();
        let log = MockLog { records: vec![] };
        let now = Utc::now();
        let last = now - chrono::Duration::hours(6);
        let r = try_compose_away_digest(&cfg, Some(last), now, &log)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn populated_log_returns_digest() {
        let cfg = enabled_cfg();
        let log = MockLog {
            records: vec![rec("done"), rec("done"), rec("failed")],
        };
        let now = Utc::now();
        let last = now - chrono::Duration::hours(6);
        let r = try_compose_away_digest(&cfg, Some(last), now, &log)
            .await
            .unwrap()
            .unwrap();
        assert!(r.contains("While you were away"));
        assert!(r.contains("3 goal turn(s)"));
        assert!(r.contains("2 completed"));
        assert!(r.contains("1 failed"));
    }

    #[test]
    fn digest_renders_completed_aborted_failed_counts() {
        let events = vec![
            rec("completed_ok"),
            rec("done"),
            rec("aborted_user"),
            rec("cancelled"),
            rec("failed_quota"),
            rec("running"), // counts as "other"
        ];
        let s = build_digest(&events, Duration::from_secs(7200), 50);
        assert!(s.contains("2h0m"));
        assert!(s.contains("6 goal turn(s)"));
        assert!(s.contains("2 completed"));
        assert!(s.contains("2 aborted/cancelled"));
        assert!(s.contains("1 failed"));
        assert!(s.contains("1 in progress / other"));
    }

    #[test]
    fn digest_caps_at_max_events() {
        let events: Vec<TurnRecord> = (0..50).map(|_| rec("done")).collect();
        let s = build_digest(&events, Duration::from_secs(3600), 50);
        assert!(s.contains("showing the most recent 50"));
    }

    #[test]
    fn digest_below_cap_no_truncation_suffix() {
        let events: Vec<TurnRecord> = (0..10).map(|_| rec("done")).collect();
        let s = build_digest(&events, Duration::from_secs(3600), 50);
        assert!(!s.contains("showing the most recent"));
    }

    #[test]
    fn digest_renders_minutes_correctly() {
        // 2h 30m elapsed
        let s = build_digest(&[rec("done")], Duration::from_secs(2 * 3600 + 30 * 60), 50);
        assert!(s.contains("2h30m"));
    }

    #[tokio::test]
    async fn populated_log_truncates_to_max_events() {
        let cfg = AwaySummaryConfig {
            enabled: true,
            threshold_hours: 4,
            max_events: 3,
        };
        let log = MockLog {
            records: vec![
                rec("done"),
                rec("done"),
                rec("done"),
                rec("done"),
                rec("done"),
            ],
        };
        let now = Utc::now();
        let last = now - chrono::Duration::hours(6);
        let r = try_compose_away_digest(&cfg, Some(last), now, &log)
            .await
            .unwrap()
            .unwrap();
        assert!(r.contains("3 goal turn(s)"));
        assert!(r.contains("showing the most recent 3"));
    }
}
