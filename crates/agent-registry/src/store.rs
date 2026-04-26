//! Persistence layer for the agent registry.
//!
//! Two implementations: an in-memory map for dev / tests, and a
//! SQLite-backed store that survives daemon restart so reattach
//! (Phase 67.B.4) can rehydrate Running goals.

use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use dashmap::DashMap;
use nexo_driver_types::GoalId;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::types::{AgentHandle, AgentRunStatus};

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Error)]
pub enum AgentRegistryStoreError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("invalid goal_id: {0}")]
    GoalId(String),
    #[error("invalid status: {0}")]
    Status(String),
    #[error("json: {0}")]
    Json(String),
}

#[async_trait]
pub trait AgentRegistryStore: Send + Sync + 'static {
    async fn upsert(&self, handle: &AgentHandle) -> Result<(), AgentRegistryStoreError>;
    async fn get(&self, goal_id: GoalId) -> Result<Option<AgentHandle>, AgentRegistryStoreError>;
    async fn list(&self) -> Result<Vec<AgentHandle>, AgentRegistryStoreError>;
    async fn list_by_status(
        &self,
        status: AgentRunStatus,
    ) -> Result<Vec<AgentHandle>, AgentRegistryStoreError>;
    /// Remove the row (used by `evict_completed`).
    async fn remove(&self, goal_id: GoalId) -> Result<(), AgentRegistryStoreError>;
    /// Bulk evict by terminal-state + age.
    async fn evict_terminal_older_than(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, AgentRegistryStoreError>;
}

/// Stub used by dev / tests. State lives only in process memory.
#[derive(Default)]
pub struct MemoryAgentRegistryStore {
    inner: DashMap<GoalId, AgentHandle>,
}

#[async_trait]
impl AgentRegistryStore for MemoryAgentRegistryStore {
    async fn upsert(&self, handle: &AgentHandle) -> Result<(), AgentRegistryStoreError> {
        self.inner.insert(handle.goal_id, handle.clone());
        Ok(())
    }

    async fn get(&self, goal_id: GoalId) -> Result<Option<AgentHandle>, AgentRegistryStoreError> {
        Ok(self.inner.get(&goal_id).map(|e| e.value().clone()))
    }

    async fn list(&self) -> Result<Vec<AgentHandle>, AgentRegistryStoreError> {
        let mut out: Vec<AgentHandle> = self.inner.iter().map(|e| e.value().clone()).collect();
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(out)
    }

    async fn list_by_status(
        &self,
        status: AgentRunStatus,
    ) -> Result<Vec<AgentHandle>, AgentRegistryStoreError> {
        let mut out: Vec<AgentHandle> = self
            .inner
            .iter()
            .filter(|e| e.value().status == status)
            .map(|e| e.value().clone())
            .collect();
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(out)
    }

    async fn remove(&self, goal_id: GoalId) -> Result<(), AgentRegistryStoreError> {
        self.inner.remove(&goal_id);
        Ok(())
    }

    async fn evict_terminal_older_than(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, AgentRegistryStoreError> {
        let victims: Vec<GoalId> = self
            .inner
            .iter()
            .filter(|e| {
                let h = e.value();
                h.status.is_terminal() && h.finished_at.map(|t| t < cutoff).unwrap_or(false)
            })
            .map(|e| *e.key())
            .collect();
        let n = victims.len() as u64;
        for g in victims {
            self.inner.remove(&g);
        }
        Ok(n)
    }
}

/// File-backed SQLite store. Schema:
///
/// ```sql
/// CREATE TABLE agent_registry (
///   goal_id      TEXT PRIMARY KEY,
///   phase_id     TEXT NOT NULL,
///   status       TEXT NOT NULL,
///   started_at   INTEGER NOT NULL,
///   finished_at  INTEGER,
///   handle_json  TEXT NOT NULL
/// );
/// ```
pub struct SqliteAgentRegistryStore {
    pool: SqlitePool,
}

impl SqliteAgentRegistryStore {
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
            "CREATE TABLE IF NOT EXISTS agent_registry (\
                goal_id      TEXT PRIMARY KEY,\
                phase_id     TEXT NOT NULL,\
                status       TEXT NOT NULL,\
                started_at   INTEGER NOT NULL,\
                finished_at  INTEGER,\
                handle_json  TEXT NOT NULL\
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_agent_registry_status \
                 ON agent_registry(status)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_agent_registry_finished \
                 ON agent_registry(finished_at)",
        )
        .execute(pool)
        .await?;
        sqlx::query(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
            .execute(pool)
            .await?;
        Ok(())
    }
}

fn unix(dt: DateTime<Utc>) -> i64 {
    dt.timestamp()
}
fn from_unix(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

fn parse_status(s: &str) -> Result<AgentRunStatus, AgentRegistryStoreError> {
    Ok(match s {
        "running" => AgentRunStatus::Running,
        "queued" => AgentRunStatus::Queued,
        "paused" => AgentRunStatus::Paused,
        "done" => AgentRunStatus::Done,
        "failed" => AgentRunStatus::Failed,
        "cancelled" => AgentRunStatus::Cancelled,
        "lost_on_restart" => AgentRunStatus::LostOnRestart,
        other => return Err(AgentRegistryStoreError::Status(other.into())),
    })
}

fn row_to_handle(row: &sqlx::sqlite::SqliteRow) -> Result<AgentHandle, AgentRegistryStoreError> {
    let json: String = row.try_get("handle_json")?;
    let mut handle: AgentHandle =
        serde_json::from_str(&json).map_err(|e| AgentRegistryStoreError::Json(e.to_string()))?;
    // Authoritative fields come back from row columns (so callers can
    // index on them efficiently); the JSON blob carries the rest.
    let goal_id_str: String = row.try_get("goal_id")?;
    let goal = Uuid::parse_str(&goal_id_str)
        .map_err(|e| AgentRegistryStoreError::GoalId(e.to_string()))?;
    handle.goal_id = GoalId(goal);
    let status: String = row.try_get("status")?;
    handle.status = parse_status(&status)?;
    let started: i64 = row.try_get("started_at")?;
    handle.started_at = from_unix(started);
    let finished: Option<i64> = row.try_get("finished_at")?;
    handle.finished_at = finished.map(from_unix);
    Ok(handle)
}

#[async_trait]
impl AgentRegistryStore for SqliteAgentRegistryStore {
    async fn upsert(&self, handle: &AgentHandle) -> Result<(), AgentRegistryStoreError> {
        let json = serde_json::to_string(handle)
            .map_err(|e| AgentRegistryStoreError::Json(e.to_string()))?;
        sqlx::query(
            "INSERT INTO agent_registry (goal_id, phase_id, status, started_at, finished_at, handle_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(goal_id) DO UPDATE SET \
                 phase_id     = excluded.phase_id, \
                 status       = excluded.status, \
                 finished_at  = excluded.finished_at, \
                 handle_json  = excluded.handle_json",
        )
        .bind(handle.goal_id.0.to_string())
        .bind(&handle.phase_id)
        .bind(handle.status.as_str())
        .bind(unix(handle.started_at))
        .bind(handle.finished_at.map(unix))
        .bind(json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, goal_id: GoalId) -> Result<Option<AgentHandle>, AgentRegistryStoreError> {
        let row = sqlx::query(
            "SELECT goal_id, phase_id, status, started_at, finished_at, handle_json \
             FROM agent_registry WHERE goal_id = ?1 LIMIT 1",
        )
        .bind(goal_id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_handle(&r)).transpose()
    }

    async fn list(&self) -> Result<Vec<AgentHandle>, AgentRegistryStoreError> {
        let rows = sqlx::query(
            "SELECT goal_id, phase_id, status, started_at, finished_at, handle_json \
             FROM agent_registry ORDER BY started_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(row_to_handle(r)?);
        }
        Ok(out)
    }

    async fn list_by_status(
        &self,
        status: AgentRunStatus,
    ) -> Result<Vec<AgentHandle>, AgentRegistryStoreError> {
        let rows = sqlx::query(
            "SELECT goal_id, phase_id, status, started_at, finished_at, handle_json \
             FROM agent_registry WHERE status = ?1 ORDER BY started_at DESC",
        )
        .bind(status.as_str())
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(row_to_handle(r)?);
        }
        Ok(out)
    }

    async fn remove(&self, goal_id: GoalId) -> Result<(), AgentRegistryStoreError> {
        sqlx::query("DELETE FROM agent_registry WHERE goal_id = ?1")
            .bind(goal_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn evict_terminal_older_than(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, AgentRegistryStoreError> {
        let res = sqlx::query(
            "DELETE FROM agent_registry \
             WHERE status IN ('done','failed','cancelled','lost_on_restart') \
               AND finished_at IS NOT NULL \
               AND finished_at < ?1",
        )
        .bind(unix(cutoff))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

// `Path` import is used by future helpers; keep the dep silenced.
const _: fn() = || {
    let _: &Path = Path::new("/");
};
