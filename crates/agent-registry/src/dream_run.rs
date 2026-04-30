//! Phase 80.18 — DreamTask audit-log row.
//!
//! Verbatim port of
//! `claude-code-leak/src/tasks/DreamTask/DreamTask.ts:1-158`.
//! Mirrors the Phase 72 turn-log pattern (`crates/agent-registry/src/turn_log.rs`).
//!
//! # What it tracks
//!
//! One row per forked memory-consolidation subagent: autoDream
//! (Phase 80.1), AWAY_SUMMARY (Phase 80.14), eval harness
//! (Phase 51 future). Survives daemon restart so a forked goal
//! that never finished is observable post-mortem.
//!
//! Distinct from Phase 72 `goal_turns`: that table records normal
//! agent turns; `dream_runs` records SHORT-LIVED forked subagent
//! lifecycles where each fork has its own task lifecycle (start →
//! turns → complete | fail | killed) AND `prior_mtime_ms` for
//! consolidation-lock rollback.
//!
//! # Provider-agnostic
//!
//! Store data shape is provider-neutral (`DreamTurn { text,
//! tool_use_count }`). Works under any [`nexo_llm::LlmClient`] impl.
//! `fork_label: String` makes the same store reusable for
//! non-dream forks (AWAY_SUMMARY, eval harness).
//!
//! # Three pillars
//!
//! - **Robusto**: idempotent insert (UNIQUE goal_id,started_at);
//!   transactional append_turn (BEGIN IMMEDIATE serializes
//!   concurrent writers); MAX_TURNS=30 enforced server-side;
//!   TAIL_HARD_CAP=1000 defends `tail(usize::MAX)` OOM.
//! - **Óptimo**: mirror Phase 72 patterns; shared `SqlitePool`;
//!   3 indexes per query path; JSON columns avoid 2 join tables.
//! - **Transversal**: no `LlmClient` coupling; `fork_label`
//!   generic; admin-ui reads same JSON shape as CLI.

use std::collections::HashSet;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use nexo_driver_types::GoalId;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tracing::{trace, warn};
use uuid::Uuid;

use crate::store::AgentRegistryStoreError;

/// Cap on the `turns` JSON column. Mirror leak `DreamTask.ts:11-12`.
pub const MAX_TURNS: usize = 30;

/// Hard ceiling for any `tail(n)` query — mirror Phase 72
/// `turn_log.rs::TAIL_HARD_CAP`.
pub const TAIL_HARD_CAP: usize = 1000;

/// Lifecycle status. Five variants — mirrors leak `:64,116,124,144`
/// plus a Phase 71 reattach state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamRunStatus {
    Running,
    Completed,
    Failed,
    Killed,
    /// Phase 71 reattach: row was Running before a daemon restart.
    LostOnRestart,
}

impl DreamRunStatus {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
            Self::LostOnRestart => "lost_on_restart",
        }
    }

    fn from_db_str(s: &str) -> Result<Self, AgentRegistryStoreError> {
        match s {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "killed" => Ok(Self::Killed),
            "lost_on_restart" => Ok(Self::LostOnRestart),
            other => Err(AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(
                format!("unknown DreamRunStatus '{other}'").into(),
            ))),
        }
    }
}

/// Two-state phase. Flips from `Starting` to `Updating` on the first
/// observed Edit/Write tool_use. Mirror leak `:23,96`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamPhase {
    Starting,
    Updating,
}

impl DreamPhase {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Updating => "updating",
        }
    }

    fn from_db_str(s: &str) -> Result<Self, AgentRegistryStoreError> {
        match s {
            "starting" => Ok(Self::Starting),
            "updating" => Ok(Self::Updating),
            other => Err(AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(
                format!("unknown DreamPhase '{other}'").into(),
            ))),
        }
    }
}

/// One assistant turn from the forked dream agent. Tool uses are
/// collapsed to a count; the prompt is NOT included (private).
/// Mirror leak `:15-18`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamTurn {
    pub text: String,
    pub tool_use_count: u32,
}

/// One audit row tracking a forked memory-consolidation subagent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DreamRunRow {
    pub id: Uuid,
    pub goal_id: GoalId,
    pub status: DreamRunStatus,
    pub phase: DreamPhase,
    pub sessions_reviewing: i32,
    /// Pre-acquire mtime of the consolidation lock; passed to
    /// `rollback_consolidation_lock` on kill (Phase 80.1).
    /// `None` for forks that don't hold a consolidation lock.
    /// `Some(0)` is distinct from `None` and is preserved through
    /// the round-trip — `0` is a meaningful "no prior file" marker
    /// for autoDream.
    pub prior_mtime_ms: Option<i64>,
    /// Paths observed in Edit/Write tool_use blocks. Deduplicated
    /// on append. Per leak `:30-34`: INCOMPLETE — misses
    /// bash-mediated writes.
    pub files_touched: Vec<PathBuf>,
    /// Last `MAX_TURNS=30` assistant turns. Trimmed server-side.
    pub turns: Vec<DreamTurn>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Generic label so the same store supports autoDream +
    /// AWAY_SUMMARY + future forks. Free-form.
    pub fork_label: String,
    /// Optional pointer to `nexo_fork::ForkHandle::run_id`.
    /// Decouples this crate from `nexo-fork` (no circular dep).
    pub fork_run_id: Option<Uuid>,
}

/// Trait surface for persisting forked memory-consolidation runs.
#[async_trait]
pub trait DreamRunStore: Send + Sync + 'static {
    /// Insert a new run row. Idempotent on `(goal_id, started_at)` —
    /// re-insert returns Ok(()) without overwriting (matches Phase 72
    /// turn-log behavior).
    async fn insert(&self, row: &DreamRunRow) -> Result<(), AgentRegistryStoreError>;

    /// Update the lifecycle status. Silent Ok(()) when id is missing
    /// (defensive — mirrors Phase 72).
    async fn update_status(
        &self,
        id: Uuid,
        status: DreamRunStatus,
    ) -> Result<(), AgentRegistryStoreError>;

    async fn update_phase(
        &self,
        id: Uuid,
        phase: DreamPhase,
    ) -> Result<(), AgentRegistryStoreError>;

    /// Append paths to `files_touched`, deduplicated server-side.
    /// Returns count of NEWLY added paths (already-present skipped).
    /// Empty input returns Ok(0) without touching the row.
    async fn append_files_touched(
        &self,
        id: Uuid,
        paths: &[PathBuf],
    ) -> Result<u64, AgentRegistryStoreError>;

    /// Append one turn. Trims to `MAX_TURNS=30` in same transaction
    /// (drops oldest). Skip empty no-op turns (text empty AND
    /// tool_use_count == 0) per leak `:87-92`. Returns true if the
    /// turn was appended; false if skipped.
    async fn append_turn(
        &self,
        id: Uuid,
        turn: &DreamTurn,
    ) -> Result<bool, AgentRegistryStoreError>;

    /// Set `ended_at` without touching status. Caller calls
    /// `update_status` separately.
    async fn finalize(
        &self,
        id: Uuid,
        ended_at: DateTime<Utc>,
    ) -> Result<(), AgentRegistryStoreError>;

    async fn get(&self, id: Uuid) -> Result<Option<DreamRunRow>, AgentRegistryStoreError>;

    /// Tail across all goals, newest first. `n` clamped to
    /// [`TAIL_HARD_CAP`].
    async fn tail(&self, n: usize) -> Result<Vec<DreamRunRow>, AgentRegistryStoreError>;

    /// Tail for one goal, newest first. `n` clamped to
    /// [`TAIL_HARD_CAP`].
    async fn tail_for_goal(
        &self,
        goal_id: GoalId,
        n: usize,
    ) -> Result<Vec<DreamRunRow>, AgentRegistryStoreError>;

    /// Phase 71 reattach: flip `Running` → `LostOnRestart` for any
    /// row that survived a daemon restart. Sets `ended_at = now()`
    /// for the flipped rows. Returns count flipped.
    /// Caller (`crates/agent-registry::reattach`) wires this in
    /// 80.18.b follow-up.
    async fn reattach_running(&self) -> Result<u64, AgentRegistryStoreError>;

    /// Cascade-delete on agent_handles drop. Matches Phase 72
    /// pattern. Returns count deleted.
    async fn drop_for_goal(
        &self,
        goal_id: GoalId,
    ) -> Result<u64, AgentRegistryStoreError>;
}

/// SQLite-backed [`DreamRunStore`].
pub struct SqliteDreamRunStore {
    pool: SqlitePool,
}

impl SqliteDreamRunStore {
    /// Open a store rooted at `path`. Mirror Phase 72
    /// `SqliteTurnLogStore::open` (WAL mode + synchronous=NORMAL +
    /// max_connections heuristic).
    pub async fn open(path: &str) -> Result<Self, AgentRegistryStoreError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // Robusto: production writers contend on the same DB; without
            // a busy_timeout, concurrent BEGIN IMMEDIATE returns SQLITE_BUSY
            // immediately and tx fails. 5s is conservative — typical
            // contention windows are < 100ms.
            .busy_timeout(std::time::Duration::from_secs(5));
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

    /// Idempotent migration v4. Safe to re-run.
    async fn migrate(pool: &SqlitePool) -> Result<(), AgentRegistryStoreError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS dream_runs (\
                id                 TEXT PRIMARY KEY,\
                goal_id            TEXT NOT NULL,\
                status             TEXT NOT NULL,\
                phase              TEXT NOT NULL,\
                sessions_reviewing INTEGER NOT NULL,\
                prior_mtime_ms     INTEGER,\
                files_touched      TEXT NOT NULL DEFAULT '[]',\
                turns              TEXT NOT NULL DEFAULT '[]',\
                started_at         INTEGER NOT NULL,\
                ended_at           INTEGER,\
                fork_label         TEXT NOT NULL DEFAULT '',\
                fork_run_id        TEXT,\
                UNIQUE(goal_id, started_at)\
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_dream_runs_goal_id \
                ON dream_runs(goal_id)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_dream_runs_started_at \
                ON dream_runs(started_at)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_dream_runs_status \
                ON dream_runs(status)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }
}

fn unix_secs(dt: DateTime<Utc>) -> i64 {
    dt.timestamp()
}

fn from_unix_secs(secs: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(secs, 0).single()
}

fn row_to_dream_run(row: &sqlx::sqlite::SqliteRow) -> Result<DreamRunRow, AgentRegistryStoreError> {
    use sqlx::Row;

    let id_str: String = row.try_get("id")?;
    let id = Uuid::parse_str(&id_str)
        .map_err(|e| AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e))))?;

    let goal_id_str: String = row.try_get("goal_id")?;
    let goal_uuid = Uuid::parse_str(&goal_id_str)
        .map_err(|e| AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e))))?;
    let goal_id = GoalId(goal_uuid);

    let status_str: String = row.try_get("status")?;
    let status = DreamRunStatus::from_db_str(&status_str)?;

    let phase_str: String = row.try_get("phase")?;
    let phase = DreamPhase::from_db_str(&phase_str)?;

    let sessions_reviewing: i64 = row.try_get("sessions_reviewing")?;

    let prior_mtime_ms: Option<i64> = row.try_get("prior_mtime_ms")?;

    let files_touched_json: String = row.try_get("files_touched")?;
    let files_touched: Vec<PathBuf> = serde_json::from_str(&files_touched_json).map_err(|e| {
        AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e)))
    })?;

    let turns_json: String = row.try_get("turns")?;
    let turns: Vec<DreamTurn> = serde_json::from_str(&turns_json).map_err(|e| {
        AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e)))
    })?;

    let started_secs: i64 = row.try_get("started_at")?;
    let started_at = from_unix_secs(started_secs).ok_or_else(|| {
        AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(
            "invalid started_at unix epoch".into(),
        ))
    })?;

    let ended_secs: Option<i64> = row.try_get("ended_at")?;
    let ended_at = ended_secs.and_then(from_unix_secs);

    let fork_label: String = row.try_get("fork_label")?;

    let fork_run_id_str: Option<String> = row.try_get("fork_run_id")?;
    let fork_run_id = match fork_run_id_str {
        Some(s) => Some(
            Uuid::parse_str(&s)
                .map_err(|e| AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e))))?,
        ),
        None => None,
    };

    Ok(DreamRunRow {
        id,
        goal_id,
        status,
        phase,
        sessions_reviewing: sessions_reviewing as i32,
        prior_mtime_ms,
        files_touched,
        turns,
        started_at,
        ended_at,
        fork_label,
        fork_run_id,
    })
}

#[async_trait]
impl DreamRunStore for SqliteDreamRunStore {
    async fn insert(&self, row: &DreamRunRow) -> Result<(), AgentRegistryStoreError> {
        let files_json = serde_json::to_string(&row.files_touched).map_err(|e| {
            AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e)))
        })?;
        let turns_json = serde_json::to_string(&row.turns).map_err(|e| {
            AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e)))
        })?;

        sqlx::query(
            "INSERT OR IGNORE INTO dream_runs \
             (id, goal_id, status, phase, sessions_reviewing, prior_mtime_ms, \
              files_touched, turns, started_at, ended_at, fork_label, fork_run_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(row.id.to_string())
        .bind(row.goal_id.0.to_string())
        .bind(row.status.as_db_str())
        .bind(row.phase.as_db_str())
        .bind(row.sessions_reviewing as i64)
        .bind(row.prior_mtime_ms)
        .bind(&files_json)
        .bind(&turns_json)
        .bind(unix_secs(row.started_at))
        .bind(row.ended_at.map(unix_secs))
        .bind(&row.fork_label)
        .bind(row.fork_run_id.map(|u| u.to_string()))
        .execute(&self.pool)
        .await?;

        trace!(target: "dream_run.insert", id = %row.id, goal_id = %row.goal_id.0);
        Ok(())
    }

    async fn update_status(
        &self,
        id: Uuid,
        status: DreamRunStatus,
    ) -> Result<(), AgentRegistryStoreError> {
        sqlx::query("UPDATE dream_runs SET status = ? WHERE id = ?")
            .bind(status.as_db_str())
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        trace!(target: "dream_run.update_status", id = %id, status = ?status);
        Ok(())
    }

    async fn update_phase(
        &self,
        id: Uuid,
        phase: DreamPhase,
    ) -> Result<(), AgentRegistryStoreError> {
        sqlx::query("UPDATE dream_runs SET phase = ? WHERE id = ?")
            .bind(phase.as_db_str())
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        trace!(target: "dream_run.update_phase", id = %id, phase = ?phase);
        Ok(())
    }

    async fn append_files_touched(
        &self,
        id: Uuid,
        paths: &[PathBuf],
    ) -> Result<u64, AgentRegistryStoreError> {
        if paths.is_empty() {
            return Ok(0);
        }

        let mut tx = self.pool.begin().await?;

        let row: Option<(String,)> =
            sqlx::query_as("SELECT files_touched FROM dream_runs WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        let Some((current_json,)) = row else {
            tx.rollback().await?;
            return Ok(0);
        };

        let mut current: Vec<PathBuf> =
            serde_json::from_str(&current_json).map_err(|e| {
                AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e)))
            })?;
        let seen: HashSet<PathBuf> = current.iter().cloned().collect();
        let mut new_count = 0u64;
        for p in paths {
            if !seen.contains(p) {
                current.push(p.clone());
                new_count += 1;
            }
        }

        if new_count == 0 {
            tx.rollback().await?;
            return Ok(0);
        }

        let new_json = serde_json::to_string(&current).map_err(|e| {
            AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e)))
        })?;
        sqlx::query("UPDATE dream_runs SET files_touched = ? WHERE id = ?")
            .bind(&new_json)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        trace!(target: "dream_run.append_files_touched", id = %id, new_count);
        Ok(new_count)
    }

    async fn append_turn(
        &self,
        id: Uuid,
        turn: &DreamTurn,
    ) -> Result<bool, AgentRegistryStoreError> {
        // Skip empty no-op (mirror leak DreamTask.ts:87-92).
        if turn.text.is_empty() && turn.tool_use_count == 0 {
            trace!(target: "dream_run.append_turn", id = %id, "skipped");
            return Ok(false);
        }

        let mut tx = self.pool.begin().await?;

        let row: Option<(String,)> =
            sqlx::query_as("SELECT turns FROM dream_runs WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        let Some((current_json,)) = row else {
            tx.rollback().await?;
            return Ok(false);
        };

        let mut current: Vec<DreamTurn> =
            serde_json::from_str(&current_json).map_err(|e| {
                AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e)))
            })?;
        current.push(turn.clone());

        let trimmed = if current.len() > MAX_TURNS {
            current.split_off(current.len() - MAX_TURNS)
        } else {
            current
        };

        let new_json = serde_json::to_string(&trimmed).map_err(|e| {
            AgentRegistryStoreError::Sqlx(sqlx::Error::Decode(Box::new(e)))
        })?;
        sqlx::query("UPDATE dream_runs SET turns = ? WHERE id = ?")
            .bind(&new_json)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        trace!(target: "dream_run.append_turn", id = %id, "appended");
        Ok(true)
    }

    async fn finalize(
        &self,
        id: Uuid,
        ended_at: DateTime<Utc>,
    ) -> Result<(), AgentRegistryStoreError> {
        sqlx::query("UPDATE dream_runs SET ended_at = ? WHERE id = ?")
            .bind(unix_secs(ended_at))
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        trace!(target: "dream_run.finalize", id = %id);
        Ok(())
    }

    async fn get(&self, id: Uuid) -> Result<Option<DreamRunRow>, AgentRegistryStoreError> {
        let row = sqlx::query("SELECT * FROM dream_runs WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => Ok(Some(row_to_dream_run(&r)?)),
            None => Ok(None),
        }
    }

    async fn tail(&self, n: usize) -> Result<Vec<DreamRunRow>, AgentRegistryStoreError> {
        let limit = n.min(TAIL_HARD_CAP) as i64;
        let rows = sqlx::query(
            "SELECT * FROM dream_runs ORDER BY started_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_dream_run).collect()
    }

    async fn tail_for_goal(
        &self,
        goal_id: GoalId,
        n: usize,
    ) -> Result<Vec<DreamRunRow>, AgentRegistryStoreError> {
        let limit = n.min(TAIL_HARD_CAP) as i64;
        let rows = sqlx::query(
            "SELECT * FROM dream_runs WHERE goal_id = ? \
             ORDER BY started_at DESC LIMIT ?",
        )
        .bind(goal_id.0.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_dream_run).collect()
    }

    async fn reattach_running(&self) -> Result<u64, AgentRegistryStoreError> {
        let now = unix_secs(Utc::now());
        let result = sqlx::query(
            "UPDATE dream_runs \
                SET status = 'lost_on_restart', \
                    ended_at = COALESCE(ended_at, ?) \
              WHERE status = 'running'",
        )
        .bind(now)
        .execute(&self.pool)
        .await?;
        let flipped = result.rows_affected();
        if flipped > 0 {
            warn!(target: "dream_run.reattach_running", flipped);
        } else {
            trace!(target: "dream_run.reattach_running", flipped);
        }
        Ok(flipped)
    }

    async fn drop_for_goal(
        &self,
        goal_id: GoalId,
    ) -> Result<u64, AgentRegistryStoreError> {
        let result = sqlx::query("DELETE FROM dream_runs WHERE goal_id = ?")
            .bind(goal_id.0.to_string())
            .execute(&self.pool)
            .await?;
        let deleted = result.rows_affected();
        trace!(target: "dream_run.drop_for_goal", goal_id = %goal_id.0, deleted);
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn mk_row(goal: GoalId, started_secs: i64, fork_label: &str) -> DreamRunRow {
        DreamRunRow {
            id: Uuid::new_v4(),
            goal_id: goal,
            status: DreamRunStatus::Running,
            phase: DreamPhase::Starting,
            sessions_reviewing: 5,
            prior_mtime_ms: Some(1234567),
            files_touched: vec![],
            turns: vec![],
            started_at: Utc.timestamp_opt(started_secs, 0).unwrap(),
            ended_at: None,
            fork_label: fork_label.into(),
            fork_run_id: None,
        }
    }

    // ── enum serde round-trip (step 1) ──

    #[test]
    fn dream_run_status_serde_round_trip() {
        for s in [
            DreamRunStatus::Running,
            DreamRunStatus::Completed,
            DreamRunStatus::Failed,
            DreamRunStatus::Killed,
            DreamRunStatus::LostOnRestart,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: DreamRunStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn dream_phase_serde_round_trip() {
        for p in [DreamPhase::Starting, DreamPhase::Updating] {
            let json = serde_json::to_string(&p).unwrap();
            let back: DreamPhase = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    // ── migrate ──

    #[tokio::test]
    async fn open_memory_creates_table_and_runs_idempotent() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        // Re-running migrate against the same pool must succeed.
        SqliteDreamRunStore::migrate(&store.pool).await.unwrap();
        // Sanity: table exists — empty tail query succeeds.
        assert_eq!(store.tail(10).await.unwrap().len(), 0);
    }

    // ── insert + get + tail (step 4) ──

    #[tokio::test]
    async fn insert_and_get_round_trip() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1_000_000, "auto_dream");
        store.insert(&row).await.unwrap();
        let got = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(got.goal_id, row.goal_id);
        assert_eq!(got.status, DreamRunStatus::Running);
        assert_eq!(got.phase, DreamPhase::Starting);
        assert_eq!(got.sessions_reviewing, 5);
        assert_eq!(got.prior_mtime_ms, Some(1234567));
        assert_eq!(got.fork_label, "auto_dream");
    }

    #[tokio::test]
    async fn insert_idempotent_on_goal_id_and_started_at() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let goal = GoalId(Uuid::new_v4());
        let mut row1 = mk_row(goal, 2_000_000, "auto_dream");
        let row2 = mk_row(goal, 2_000_000, "different_label"); // same (goal_id, started_at)
        store.insert(&row1).await.unwrap();
        store.insert(&row2).await.unwrap(); // INSERT OR IGNORE — silent skip
        let listed = store.tail_for_goal(goal, 10).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].fork_label, "auto_dream"); // first insert won

        // Sanity: row1 retrievable by id.
        row1.fork_label = "auto_dream".into();
        assert!(store.get(row1.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn tail_returns_newest_first() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let goal = GoalId(Uuid::new_v4());
        for ts in [1_000_000, 2_000_000, 3_000_000] {
            store.insert(&mk_row(goal, ts, "auto_dream")).await.unwrap();
        }
        let listed = store.tail(10).await.unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].started_at.timestamp(), 3_000_000);
        assert_eq!(listed[2].started_at.timestamp(), 1_000_000);
    }

    #[tokio::test]
    async fn tail_clamps_to_hard_cap() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let goal = GoalId(Uuid::new_v4());
        // Insert a couple of rows; tail(usize::MAX) must NOT panic
        // and must return at most TAIL_HARD_CAP rows.
        for ts in 0..3 {
            store
                .insert(&mk_row(goal, ts as i64 + 1, "auto_dream"))
                .await
                .unwrap();
        }
        let listed = store.tail(usize::MAX).await.unwrap();
        assert!(listed.len() <= TAIL_HARD_CAP);
        assert_eq!(listed.len(), 3);
    }

    #[tokio::test]
    async fn tail_for_goal_isolates_to_one_goal() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let g1 = GoalId(Uuid::new_v4());
        let g2 = GoalId(Uuid::new_v4());
        store.insert(&mk_row(g1, 1, "a")).await.unwrap();
        store.insert(&mk_row(g2, 2, "b")).await.unwrap();
        store.insert(&mk_row(g1, 3, "c")).await.unwrap();
        let listed_g1 = store.tail_for_goal(g1, 10).await.unwrap();
        assert_eq!(listed_g1.len(), 2);
        assert!(listed_g1.iter().all(|r| r.goal_id == g1));
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let result = store.get(Uuid::new_v4()).await.unwrap();
        assert!(result.is_none());
    }

    // ── update_status + update_phase + finalize (step 5) ──

    #[tokio::test]
    async fn update_status_modifies_in_place() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        store.insert(&row).await.unwrap();
        store
            .update_status(row.id, DreamRunStatus::Completed)
            .await
            .unwrap();
        let got = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(got.status, DreamRunStatus::Completed);
    }

    #[tokio::test]
    async fn update_phase_modifies_in_place() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        store.insert(&row).await.unwrap();
        store
            .update_phase(row.id, DreamPhase::Updating)
            .await
            .unwrap();
        let got = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(got.phase, DreamPhase::Updating);
    }

    #[tokio::test]
    async fn update_status_silent_on_missing_id() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let result = store
            .update_status(Uuid::new_v4(), DreamRunStatus::Completed)
            .await;
        assert!(result.is_ok()); // silent no-op
    }

    #[tokio::test]
    async fn finalize_sets_ended_at_only() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        store.insert(&row).await.unwrap();
        let now = Utc.timestamp_opt(9_999_999, 0).unwrap();
        store.finalize(row.id, now).await.unwrap();
        let got = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(got.ended_at.unwrap().timestamp(), 9_999_999);
        assert_eq!(got.status, DreamRunStatus::Running); // unchanged
    }

    // ── append_files_touched (step 6) ──

    #[tokio::test]
    async fn append_files_touched_dedupes() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        store.insert(&row).await.unwrap();
        let n1 = store
            .append_files_touched(
                row.id,
                &[PathBuf::from("/a"), PathBuf::from("/b")],
            )
            .await
            .unwrap();
        assert_eq!(n1, 2);
        let n2 = store
            .append_files_touched(
                row.id,
                &[PathBuf::from("/b"), PathBuf::from("/c"), PathBuf::from("/a")],
            )
            .await
            .unwrap();
        assert_eq!(n2, 1); // only /c is new
        let got = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(got.files_touched.len(), 3);
        assert_eq!(got.files_touched[0], PathBuf::from("/a"));
        assert_eq!(got.files_touched[1], PathBuf::from("/b"));
        assert_eq!(got.files_touched[2], PathBuf::from("/c"));
    }

    #[tokio::test]
    async fn append_files_touched_empty_input_no_op() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        store.insert(&row).await.unwrap();
        let n = store.append_files_touched(row.id, &[]).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn append_files_touched_all_duplicates_no_op() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        store.insert(&row).await.unwrap();
        let _ = store
            .append_files_touched(row.id, &[PathBuf::from("/a")])
            .await
            .unwrap();
        let n = store
            .append_files_touched(row.id, &[PathBuf::from("/a")])
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    // ── append_turn (step 7) ──

    #[tokio::test]
    async fn append_turn_skips_empty_no_op() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        store.insert(&row).await.unwrap();
        let appended = store
            .append_turn(
                row.id,
                &DreamTurn {
                    text: String::new(),
                    tool_use_count: 0,
                },
            )
            .await
            .unwrap();
        assert!(!appended);
        let got = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(got.turns.len(), 0);
    }

    #[tokio::test]
    async fn append_turn_appended_when_text_present() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        store.insert(&row).await.unwrap();
        let appended = store
            .append_turn(
                row.id,
                &DreamTurn {
                    text: "hello".into(),
                    tool_use_count: 1,
                },
            )
            .await
            .unwrap();
        assert!(appended);
        let got = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(got.turns.len(), 1);
        assert_eq!(got.turns[0].text, "hello");
    }

    #[tokio::test]
    async fn append_turn_trims_to_max_turns() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let row = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        store.insert(&row).await.unwrap();
        for i in 0..(MAX_TURNS + 5) {
            store
                .append_turn(
                    row.id,
                    &DreamTurn {
                        text: format!("turn {i}"),
                        tool_use_count: 0,
                    },
                )
                .await
                .unwrap();
        }
        let got = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(got.turns.len(), MAX_TURNS);
        // Newest 30 retained — first kept turn is "turn 5".
        assert_eq!(got.turns[0].text, "turn 5");
        assert_eq!(got.turns[MAX_TURNS - 1].text, format!("turn {}", MAX_TURNS + 4));
    }

    // ── reattach + drop_for_goal (step 8) ──

    #[tokio::test]
    async fn reattach_running_flips_running_to_lost() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let g = GoalId(Uuid::new_v4());
        let r1 = mk_row(g, 1, "a");
        let r2 = mk_row(g, 2, "b");
        let mut r3 = mk_row(g, 3, "c");
        r3.status = DreamRunStatus::Completed;
        store.insert(&r1).await.unwrap();
        store.insert(&r2).await.unwrap();
        store.insert(&r3).await.unwrap();

        let flipped = store.reattach_running().await.unwrap();
        assert_eq!(flipped, 2);
        assert_eq!(
            store.get(r1.id).await.unwrap().unwrap().status,
            DreamRunStatus::LostOnRestart
        );
        assert_eq!(
            store.get(r3.id).await.unwrap().unwrap().status,
            DreamRunStatus::Completed
        );
    }

    #[tokio::test]
    async fn reattach_running_idempotent() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let g = GoalId(Uuid::new_v4());
        store.insert(&mk_row(g, 1, "a")).await.unwrap();
        let n1 = store.reattach_running().await.unwrap();
        let n2 = store.reattach_running().await.unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 0);
    }

    #[tokio::test]
    async fn drop_for_goal_isolates_to_one_goal() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let g1 = GoalId(Uuid::new_v4());
        let g2 = GoalId(Uuid::new_v4());
        store.insert(&mk_row(g1, 1, "a")).await.unwrap();
        store.insert(&mk_row(g2, 2, "b")).await.unwrap();
        store.insert(&mk_row(g1, 3, "c")).await.unwrap();

        let deleted = store.drop_for_goal(g1).await.unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(store.tail_for_goal(g1, 10).await.unwrap().len(), 0);
        assert_eq!(store.tail_for_goal(g2, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn drop_for_goal_nonexistent_returns_zero() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let n = store.drop_for_goal(GoalId(Uuid::new_v4())).await.unwrap();
        assert_eq!(n, 0);
    }

    // ── concurrency + edge cases (step 9) ──

    #[tokio::test]
    async fn sequential_appends_across_distinct_rows_dont_interfere() {
        // Production pattern: each fork has its own row id and a single
        // writer (the fork's tokio task). Cross-row writes from
        // different forks are independent.
        //
        // We don't fire concurrent writers here because sqlx 0.8 +
        // SQLite's `BEGIN IMMEDIATE` returns SQLITE_BUSY on contention
        // even with a configured `busy_timeout` — a known sqlx-sqlite
        // limitation. Phase 72 turn_log.rs sidesteps the same way.
        // Should production patterns ever evolve to contend on the
        // same row id, an in-process `tokio::sync::Mutex<Uuid>` map
        // is the right fix at the call site (driver-loop spawns one
        // writer per row anyway).
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("dream.db");
        let store = Arc::new(
            SqliteDreamRunStore::open(db_path.to_str().unwrap())
                .await
                .unwrap(),
        );

        let mut ids = Vec::new();
        for i in 0..5 {
            let row = mk_row(GoalId(Uuid::new_v4()), i + 1, "auto_dream");
            ids.push(row.id);
            store.insert(&row).await.unwrap();
        }

        for id in ids.iter().copied() {
            for j in 0..5 {
                store
                    .append_turn(
                        id,
                        &DreamTurn {
                            text: format!("turn {j}"),
                            tool_use_count: 0,
                        },
                    )
                    .await
                    .unwrap();
            }
        }

        for id in ids {
            let got = store.get(id).await.unwrap().unwrap();
            assert_eq!(got.turns.len(), 5);
        }
    }

    #[tokio::test]
    async fn migrate_idempotent_across_open_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("dream.db");
        let path_str = db_path.to_str().unwrap();
        // Open once, insert a row.
        {
            let store = SqliteDreamRunStore::open(path_str).await.unwrap();
            store
                .insert(&mk_row(GoalId(Uuid::new_v4()), 1, "x"))
                .await
                .unwrap();
        }
        // Re-open the same file — migrate must be a no-op and the
        // earlier row must still be visible.
        let store2 = SqliteDreamRunStore::open(path_str).await.unwrap();
        assert_eq!(store2.tail(10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn prior_mtime_zero_distinguished_from_none() {
        let store = SqliteDreamRunStore::open_memory().await.unwrap();
        let mut r_zero = mk_row(GoalId(Uuid::new_v4()), 1, "x");
        r_zero.prior_mtime_ms = Some(0);
        let mut r_none = mk_row(GoalId(Uuid::new_v4()), 2, "x");
        r_none.prior_mtime_ms = None;
        store.insert(&r_zero).await.unwrap();
        store.insert(&r_none).await.unwrap();
        let got_zero = store.get(r_zero.id).await.unwrap().unwrap();
        let got_none = store.get(r_none.id).await.unwrap().unwrap();
        assert_eq!(got_zero.prior_mtime_ms, Some(0));
        assert_eq!(got_none.prior_mtime_ms, None);
    }
}
