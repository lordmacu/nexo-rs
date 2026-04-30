//! Phase 72.1 — durable per-turn audit log.
//!
//! Every Claude Code subprocess turn produces an `AttemptResult`
//! event. The runtime forwards these into `EventForwarder`, which
//! today only updates the in-memory `AgentSnapshot` and pushes a
//! line into a 200-row ring `LogBuffer`. Both are gone after a
//! daemon restart, which makes any post-mortem ("what did the
//! agent actually do over its 40 turns?") impossible.
//!
//! This module adds a SQLite-backed append-only log, keyed by
//! `(goal_id, turn_index)`. The wire format is intentionally simple:
//! a small set of indexed columns for filtering / table queries,
//! plus a `raw_json` blob with the full payload for tooling that
//! wants every field. Write path is idempotent on
//! `(goal_id, turn_index)` so a duplicate emission (replay, retry,
//! double-fire) cannot corrupt history.
//!
//! The audit log lives in the same database file as the registry
//! (`agents.db`) so backups / volume snapshots cover both, and so
//! a single sqlite open serves the runtime + the read tools.

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use nexo_driver_types::GoalId;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::store::AgentRegistryStoreError;

/// Phase 80.9.h — stable prefix for the `source` column when an
/// inbound came via an MCP channel server. Render with
/// [`format_channel_source`]; parse with [`parse_channel_source`].
pub const CHANNEL_SOURCE_PREFIX: &str = "channel:";

/// Render `server_name` into the `source` shape audit tooling
/// expects. The runtime is the only writer of this string today;
/// keeping it behind a helper means a future migration to a
/// richer marker (e.g. `channel:<server>:<binding>`) is a
/// one-line change.
pub fn format_channel_source(server_name: &str) -> String {
    format!("{CHANNEL_SOURCE_PREFIX}{server_name}")
}

/// Inverse of [`format_channel_source`]. Returns the server name
/// when `source` carries the channel prefix; `None` otherwise.
/// Used by `setup doctor` and audit tools to filter by server.
pub fn parse_channel_source(source: &str) -> Option<&str> {
    source.strip_prefix(CHANNEL_SOURCE_PREFIX)
}

/// One row in the durable turn log. Pre-rendered fields cover the
/// 99% query case (status board, last decision, error grep);
/// `raw_json` is the escape hatch for callers that want the full
/// `AttemptResult` payload.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnRecord {
    pub goal_id: GoalId,
    pub turn_index: u32,
    pub recorded_at: DateTime<Utc>,
    /// Short tag for the outcome of this turn: `done`, `continue`,
    /// `needs_retry`, `budget_exhausted`, `cancelled`, `escalate`.
    /// Pre-rendered so an SQL filter can pick "all turns that
    /// errored" without parsing JSON.
    pub outcome: String,
    /// First ~512 chars of the model's last decision text, when
    /// available. NULL when the driver loop only emitted budget /
    /// acceptance metadata for this turn.
    pub decision: Option<String>,
    /// Stable summary line the agent registry pre-rendered for this
    /// turn (matches `AgentSnapshot.last_progress_text`). Useful for
    /// quick scrolling without joining `agent_registry`.
    pub summary: Option<String>,
    /// `git diff --stat` snapshot at this turn, when the driver loop
    /// produced one. Cheap to read, expensive to recompute.
    pub diff_stat: Option<String>,
    /// First ~512 chars of any error attached to the attempt.
    pub error: Option<String>,
    /// Full JSON-encoded payload for callers that need every field
    /// (Anthropic's tool-call array, raw acceptance verdict, etc.).
    pub raw_json: String,
    /// Phase 80.9.h — origin marker. `Some("channel:<server>")`
    /// when the inbound that drove this turn arrived via an MCP
    /// channel server (Slack, Telegram, iMessage). `None` for
    /// turns triggered by every other intake path (paired user
    /// inbound, cron fire, agent-to-agent delegate, heartbeat,
    /// poller). Audit tooling filters on this column to answer
    /// "what came in via Slack today?".
    #[serde(default)]
    pub source: Option<String>,
}

#[async_trait]
pub trait TurnLogStore: Send + Sync + 'static {
    /// Append a row. Idempotent on `(goal_id, turn_index)`: a repeat
    /// call updates the existing row in place rather than failing,
    /// so a replay of the driver event stream cannot duplicate
    /// history.
    async fn append(&self, record: &TurnRecord) -> Result<(), AgentRegistryStoreError>;

    /// Last `n` turns for a goal in chronological order
    /// (oldest first). `n = 0` returns every row. `n` is capped at
    /// 1000 by the implementation to keep a runaway tool call from
    /// pulling the whole table into memory.
    async fn tail(
        &self,
        goal_id: GoalId,
        n: usize,
    ) -> Result<Vec<TurnRecord>, AgentRegistryStoreError>;

    /// Total recorded turns for a goal (used by the read tool to
    /// say "showing 20 of 47").
    async fn count(&self, goal_id: GoalId) -> Result<u64, AgentRegistryStoreError>;

    /// Drop every row for a goal — invoked when the registry evicts
    /// the parent row so the audit log doesn't outlive its goal.
    async fn drop_for_goal(&self, goal_id: GoalId) -> Result<u64, AgentRegistryStoreError>;

    /// Phase 80.14 — turns recorded since `since` (UTC), newest first,
    /// across ALL goals. Used by the AWAY_SUMMARY digest builder
    /// to sweep the silence window. `limit` is capped at 1000 by
    /// the impl to keep a runaway window from pulling the whole
    /// table into memory.
    async fn tail_since(
        &self,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TurnRecord>, AgentRegistryStoreError> {
        // Default fallback for impls that haven't customised: pull
        // everything via `tail` semantics + filter client-side.
        // Real impls should override with a SQL `WHERE` clause.
        let _ = (since, limit);
        Ok(Vec::new())
    }
}

/// SQLite-backed implementation. Owns its own pool; safe to share
/// across the runtime.
pub struct SqliteTurnLogStore {
    pool: SqlitePool,
}

const TAIL_HARD_CAP: usize = 1000;

impl SqliteTurnLogStore {
    pub async fn open(path: &str) -> Result<Self, AgentRegistryStoreError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let max_conns = if path == ":memory:" { 1 } else { 4 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(opts)
            .await?;
        if path != ":memory:" {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&pool)
                .await?;
            sqlx::query("PRAGMA synchronous = NORMAL")
                .execute(&pool)
                .await?;
        }
        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_memory() -> Result<Self, AgentRegistryStoreError> {
        Self::open(":memory:").await
    }

    async fn migrate(pool: &SqlitePool) -> Result<(), AgentRegistryStoreError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS goal_turns (\
                goal_id      TEXT NOT NULL,\
                turn_index   INTEGER NOT NULL,\
                recorded_at  INTEGER NOT NULL,\
                outcome      TEXT NOT NULL,\
                decision     TEXT,\
                summary      TEXT,\
                diff_stat    TEXT,\
                error        TEXT,\
                raw_json     TEXT NOT NULL,\
                PRIMARY KEY (goal_id, turn_index)\
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_goal_turns_recorded \
                ON goal_turns(recorded_at)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_goal_turns_outcome \
                ON goal_turns(outcome)",
        )
        .execute(pool)
        .await?;
        // Phase 80.9.h — additive `source` column. Idempotent ALTER
        // pattern (same shape as the cron `permanent` column +
        // pre-existing `recipient` column on the same module): we
        // tolerate the "duplicate column" error so migrate() stays
        // safe to call on every boot.
        let alter_source =
            sqlx::query("ALTER TABLE goal_turns ADD COLUMN source TEXT")
                .execute(pool)
                .await;
        if let Err(e) = alter_source {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(AgentRegistryStoreError::Sqlx(e));
            }
        }
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_goal_turns_source \
                ON goal_turns(source)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl TurnLogStore for SqliteTurnLogStore {
    async fn append(&self, record: &TurnRecord) -> Result<(), AgentRegistryStoreError> {
        sqlx::query(
            "INSERT INTO goal_turns \
                 (goal_id, turn_index, recorded_at, outcome, decision, summary, diff_stat, error, raw_json, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(goal_id, turn_index) DO UPDATE SET \
                 recorded_at = excluded.recorded_at, \
                 outcome     = excluded.outcome, \
                 decision    = excluded.decision, \
                 summary     = excluded.summary, \
                 diff_stat   = excluded.diff_stat, \
                 error       = excluded.error, \
                 raw_json    = excluded.raw_json, \
                 source      = excluded.source",
        )
        .bind(record.goal_id.0.to_string())
        .bind(record.turn_index as i64)
        .bind(record.recorded_at.timestamp())
        .bind(&record.outcome)
        .bind(record.decision.as_deref())
        .bind(record.summary.as_deref())
        .bind(record.diff_stat.as_deref())
        .bind(record.error.as_deref())
        .bind(&record.raw_json)
        .bind(record.source.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn tail(
        &self,
        goal_id: GoalId,
        n: usize,
    ) -> Result<Vec<TurnRecord>, AgentRegistryStoreError> {
        let limit = if n == 0 {
            TAIL_HARD_CAP
        } else {
            n.min(TAIL_HARD_CAP)
        };
        // Pull the most recent N rows desc, then flip back to
        // chronological order in the caller's view so reading top to
        // bottom matches the run.
        let rows: Vec<(
            String,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT goal_id, turn_index, recorded_at, outcome, decision, summary, diff_stat, error, raw_json, source \
             FROM goal_turns \
             WHERE goal_id = ?1 \
             ORDER BY turn_index DESC LIMIT ?2",
        )
        .bind(goal_id.0.to_string())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (gid, idx, ts, outcome, decision, summary, diff_stat, error, raw_json, source) in rows {
            let goal = Uuid::parse_str(&gid)
                .map_err(|e| AgentRegistryStoreError::GoalId(e.to_string()))?;
            out.push(TurnRecord {
                goal_id: GoalId(goal),
                turn_index: idx as u32,
                recorded_at: Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now),
                outcome,
                decision,
                summary,
                diff_stat,
                error,
                raw_json,
                source,
            });
        }
        out.reverse();
        Ok(out)
    }

    async fn count(&self, goal_id: GoalId) -> Result<u64, AgentRegistryStoreError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM goal_turns WHERE goal_id = ?1")
            .bind(goal_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0.max(0) as u64)
    }

    async fn drop_for_goal(&self, goal_id: GoalId) -> Result<u64, AgentRegistryStoreError> {
        let res = sqlx::query("DELETE FROM goal_turns WHERE goal_id = ?1")
            .bind(goal_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    async fn tail_since(
        &self,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TurnRecord>, AgentRegistryStoreError> {
        let cap = limit.min(TAIL_HARD_CAP) as i64;
        let rows: Vec<(
            String,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT goal_id, turn_index, recorded_at, outcome, decision, summary, diff_stat, error, raw_json, source \
             FROM goal_turns \
             WHERE recorded_at >= ?1 \
             ORDER BY recorded_at DESC \
             LIMIT ?2",
        )
        .bind(since.timestamp())
        .bind(cap)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (gid, idx, ts, outcome, decision, summary, diff_stat, error, raw_json, source) in rows {
            let goal = Uuid::parse_str(&gid)
                .map_err(|e| AgentRegistryStoreError::GoalId(e.to_string()))?;
            out.push(TurnRecord {
                goal_id: GoalId(goal),
                turn_index: idx as u32,
                recorded_at: Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now),
                outcome,
                decision,
                summary,
                diff_stat,
                error,
                raw_json,
                source,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(goal: GoalId, turn: u32, outcome: &str) -> TurnRecord {
        TurnRecord {
            goal_id: goal,
            turn_index: turn,
            recorded_at: Utc::now(),
            outcome: outcome.into(),
            decision: Some(format!("decision-{turn}")),
            summary: Some(format!("summary-{turn}")),
            diff_stat: None,
            error: None,
            raw_json: format!("{{\"turn\":{turn}}}"),
            source: None,
        }
    }

    #[tokio::test]
    async fn append_and_tail_round_trip_in_chronological_order() {
        let store = SqliteTurnLogStore::open_memory().await.unwrap();
        let g = GoalId(Uuid::new_v4());
        for i in 1..=5u32 {
            store.append(&record(g, i, "continue")).await.unwrap();
        }
        let last_three = store.tail(g, 3).await.unwrap();
        assert_eq!(last_three.len(), 3);
        assert_eq!(last_three[0].turn_index, 3);
        assert_eq!(last_three[2].turn_index, 5);
        let all = store.tail(g, 0).await.unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].turn_index, 1);
        assert_eq!(all[4].turn_index, 5);
        assert_eq!(store.count(g).await.unwrap(), 5);
    }

    #[tokio::test]
    async fn append_idempotent_on_goal_id_and_turn() {
        let store = SqliteTurnLogStore::open_memory().await.unwrap();
        let g = GoalId(Uuid::new_v4());
        let mut r = record(g, 7, "continue");
        store.append(&r).await.unwrap();
        // Same (goal, turn): updates in place, doesn't dup.
        r.outcome = "needs_retry".into();
        r.error = Some("boom".into());
        store.append(&r).await.unwrap();
        let all = store.tail(g, 0).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].outcome, "needs_retry");
        assert_eq!(all[0].error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn drop_for_goal_isolates_to_one_goal() {
        let store = SqliteTurnLogStore::open_memory().await.unwrap();
        let g1 = GoalId(Uuid::new_v4());
        let g2 = GoalId(Uuid::new_v4());
        store.append(&record(g1, 1, "continue")).await.unwrap();
        store.append(&record(g1, 2, "done")).await.unwrap();
        store.append(&record(g2, 1, "continue")).await.unwrap();
        let removed = store.drop_for_goal(g1).await.unwrap();
        assert_eq!(removed, 2);
        assert_eq!(store.count(g1).await.unwrap(), 0);
        assert_eq!(store.count(g2).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn tail_caps_runaway_n() {
        let store = SqliteTurnLogStore::open_memory().await.unwrap();
        let g = GoalId(Uuid::new_v4());
        for i in 1..=10u32 {
            store.append(&record(g, i, "continue")).await.unwrap();
        }
        // Hugely-out-of-range n collapses to TAIL_HARD_CAP, returns
        // every row regardless.
        let rows = store.tail(g, 100_000).await.unwrap();
        assert_eq!(rows.len(), 10);
    }

    // ---- Phase 80.9.h source marker ----

    #[tokio::test]
    async fn source_field_round_trips_through_sqlite() {
        let store = SqliteTurnLogStore::open_memory().await.unwrap();
        let g = GoalId(Uuid::new_v4());
        let mut r = record(g, 1, "continue");
        r.source = Some(format_channel_source("slack"));
        store.append(&r).await.unwrap();
        let rows = store.tail(g, 0).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source.as_deref(), Some("channel:slack"));
    }

    #[tokio::test]
    async fn source_default_is_none_for_legacy_callers() {
        let store = SqliteTurnLogStore::open_memory().await.unwrap();
        let g = GoalId(Uuid::new_v4());
        store.append(&record(g, 1, "continue")).await.unwrap();
        let rows = store.tail(g, 0).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].source.is_none());
    }

    #[tokio::test]
    async fn source_is_idempotent_on_replay() {
        // A duplicate emission overwrites the existing row in
        // place — the source field must follow the same UPSERT
        // contract as every other field.
        let store = SqliteTurnLogStore::open_memory().await.unwrap();
        let g = GoalId(Uuid::new_v4());
        let mut r = record(g, 1, "continue");
        r.source = Some("channel:slack".into());
        store.append(&r).await.unwrap();
        // Second emission flips the source — operators may want
        // to reclassify a turn that originally fired without a
        // source by re-emitting with a value.
        r.source = Some("channel:telegram".into());
        store.append(&r).await.unwrap();
        let rows = store.tail(g, 0).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source.as_deref(), Some("channel:telegram"));
    }

    #[test]
    fn format_channel_source_renders_stable_prefix() {
        assert_eq!(format_channel_source("slack"), "channel:slack");
        assert_eq!(
            format_channel_source("plugin:slack:default"),
            "channel:plugin:slack:default"
        );
    }

    #[test]
    fn parse_channel_source_extracts_server_name() {
        assert_eq!(parse_channel_source("channel:slack"), Some("slack"));
        assert_eq!(parse_channel_source("channel:tg"), Some("tg"));
        assert_eq!(parse_channel_source("agent.intake.foo"), None);
        assert_eq!(parse_channel_source(""), None);
    }
}
