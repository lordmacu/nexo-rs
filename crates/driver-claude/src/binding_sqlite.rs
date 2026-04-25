//! SQLite-backed `SessionBindingStore`. See spec for Phase 67.2.
//!
//! Schema lives in `claude_session_bindings`. WAL mode is enabled on
//! file-backed databases; `:memory:` skips it (the WAL pragma errors
//! there). `PRAGMA user_version = 1` is the migration sentinel — Phase
//! 67.x will bump it when the schema evolves.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use nexo_driver_types::GoalId;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::binding::{SessionBinding, SessionBindingStore};
use crate::error::ClaudeError;

const SCHEMA_VERSION: i64 = 1;

pub struct SqliteBindingStore {
    pool: SqlitePool,
    idle_ttl: Option<Duration>,
    max_age: Option<Duration>,
}

impl SqliteBindingStore {
    /// Open a file-backed store. Creates the file + schema on first
    /// open; subsequent calls reuse the existing tables (idempotent
    /// migration).
    pub async fn open(path: &str) -> Result<Self, ClaudeError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let max_conns = if path == ":memory:" { 1 } else { 4 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(opts)
            .await?;

        // WAL is unsupported on `:memory:` — skip if so.
        if path != ":memory:" {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&pool)
                .await?;
            sqlx::query("PRAGMA synchronous = NORMAL")
                .execute(&pool)
                .await?;
        }

        Self::migrate(&pool).await?;

        Ok(Self {
            pool,
            idle_ttl: None,
            max_age: None,
        })
    }

    /// Open an in-memory store. `max_connections=1` because
    /// `:memory:` databases are per-connection in SQLite.
    pub async fn open_memory() -> Result<Self, ClaudeError> {
        Self::open(":memory:").await
    }

    /// Configure idle-TTL filtering. `Duration::ZERO` is treated as
    /// "no filter" (same as not calling this builder).
    pub fn with_idle_ttl(mut self, ttl: Duration) -> Self {
        self.idle_ttl = if ttl.is_zero() { None } else { Some(ttl) };
        self
    }

    /// Configure max-age filtering. `Duration::ZERO` is treated as
    /// "no filter".
    pub fn with_max_age(mut self, age: Duration) -> Self {
        self.max_age = if age.is_zero() { None } else { Some(age) };
        self
    }

    /// Test-only — direct pool access. Hidden from rustdoc; not part
    /// of the public stability contract.
    #[doc(hidden)]
    pub fn pool_for_test(&self) -> &SqlitePool {
        &self.pool
    }

    async fn migrate(pool: &SqlitePool) -> Result<(), ClaudeError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS claude_session_bindings (\
                goal_id              TEXT    PRIMARY KEY,\
                session_id           TEXT    NOT NULL,\
                model                TEXT,\
                workspace            TEXT,\
                schema_version       INTEGER NOT NULL DEFAULT 1,\
                last_session_invalid INTEGER NOT NULL DEFAULT 0,\
                created_at           INTEGER NOT NULL,\
                updated_at           INTEGER NOT NULL,\
                last_active_at       INTEGER NOT NULL\
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_csb_last_active \
                 ON claude_session_bindings(last_active_at)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_csb_updated \
                 ON claude_session_bindings(updated_at)",
        )
        .execute(pool)
        .await?;
        sqlx::query(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
            .execute(pool)
            .await?;
        Ok(())
    }

    /// `(idle_floor, max_age_floor)` Unix-second cutoffs for the TTL
    /// filter parameters. Returns `(None, None)` when no filtering is
    /// configured.
    fn ttl_cutoffs_secs(&self) -> (Option<i64>, Option<i64>) {
        let now = Utc::now().timestamp();
        let idle = self.idle_ttl.map(|d| now - d.as_secs() as i64);
        let max_age = self.max_age.map(|d| now - d.as_secs() as i64);
        (idle, max_age)
    }
}

fn goal_id_str(g: GoalId) -> String {
    g.0.to_string()
}

fn parse_goal_id(s: &str) -> Result<GoalId, ClaudeError> {
    Uuid::parse_str(s)
        .map(GoalId)
        .map_err(|e| ClaudeError::Binding(format!("invalid goal_id in row: {e}")))
}

fn unix(dt: DateTime<Utc>) -> i64 {
    dt.timestamp()
}
fn from_unix(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

fn row_to_binding(row: &sqlx::sqlite::SqliteRow) -> Result<SessionBinding, ClaudeError> {
    let goal_id: String = row.try_get("goal_id")?;
    let session_id: String = row.try_get("session_id")?;
    let model: Option<String> = row.try_get("model")?;
    let workspace: Option<String> = row.try_get("workspace")?;
    let created_at: i64 = row.try_get("created_at")?;
    let updated_at: i64 = row.try_get("updated_at")?;
    let last_active_at: i64 = row.try_get("last_active_at")?;
    Ok(SessionBinding {
        goal_id: parse_goal_id(&goal_id)?,
        session_id,
        model,
        workspace: workspace.map(Into::into),
        created_at: from_unix(created_at),
        updated_at: from_unix(updated_at),
        last_active_at: from_unix(last_active_at),
    })
}

#[async_trait]
impl SessionBindingStore for SqliteBindingStore {
    async fn get(&self, goal_id: GoalId) -> Result<Option<SessionBinding>, ClaudeError> {
        let (idle_floor, max_age_floor) = self.ttl_cutoffs_secs();
        let row = sqlx::query(
            "SELECT goal_id, session_id, model, workspace, \
                    created_at, updated_at, last_active_at \
             FROM claude_session_bindings \
             WHERE goal_id = ?1 \
               AND last_session_invalid = 0 \
               AND (?2 IS NULL OR last_active_at >= ?2) \
               AND (?3 IS NULL OR created_at      >= ?3) \
             LIMIT 1",
        )
        .bind(goal_id_str(goal_id))
        .bind(idle_floor)
        .bind(max_age_floor)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_binding(&r)).transpose()
    }

    async fn upsert(&self, mut binding: SessionBinding) -> Result<(), ClaudeError> {
        let now = Utc::now();
        // Caller-supplied timestamps are normalised: structural write
        // bumps both `updated_at` and `last_active_at`.
        binding.updated_at = now;
        binding.last_active_at = now;
        // `created_at` is preserved by the ON CONFLICT clause.

        let workspace_str: Option<String> =
            binding.workspace.as_ref().map(|p| p.display().to_string());

        sqlx::query(
            "INSERT INTO claude_session_bindings ( \
                 goal_id, session_id, model, workspace, \
                 schema_version, last_session_invalid, \
                 created_at, updated_at, last_active_at \
             ) VALUES (?1, ?2, ?3, ?4, 1, 0, ?5, ?6, ?7) \
             ON CONFLICT(goal_id) DO UPDATE SET \
                 session_id           = excluded.session_id, \
                 model                = excluded.model, \
                 workspace            = excluded.workspace, \
                 last_session_invalid = 0, \
                 updated_at           = excluded.updated_at, \
                 last_active_at       = excluded.last_active_at",
        )
        .bind(goal_id_str(binding.goal_id))
        .bind(&binding.session_id)
        .bind(binding.model.as_deref())
        .bind(workspace_str)
        .bind(unix(binding.created_at))
        .bind(unix(binding.updated_at))
        .bind(unix(binding.last_active_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear(&self, goal_id: GoalId) -> Result<(), ClaudeError> {
        sqlx::query("DELETE FROM claude_session_bindings WHERE goal_id = ?1")
            .bind(goal_id_str(goal_id))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn mark_invalid(&self, goal_id: GoalId) -> Result<(), ClaudeError> {
        let now = unix(Utc::now());
        sqlx::query(
            "UPDATE claude_session_bindings \
                SET last_session_invalid = 1, updated_at = ?2 \
              WHERE goal_id = ?1",
        )
        .bind(goal_id_str(goal_id))
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn touch(&self, goal_id: GoalId) -> Result<(), ClaudeError> {
        let now = unix(Utc::now());
        sqlx::query(
            "UPDATE claude_session_bindings \
                SET last_active_at = ?2 \
              WHERE goal_id = ?1 \
                AND last_session_invalid = 0",
        )
        .bind(goal_id_str(goal_id))
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn purge_older_than(&self, cutoff: DateTime<Utc>) -> Result<u64, ClaudeError> {
        let res = sqlx::query("DELETE FROM claude_session_bindings WHERE last_active_at < ?1")
            .bind(unix(cutoff))
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    async fn list_active(&self) -> Result<Vec<SessionBinding>, ClaudeError> {
        let (idle_floor, max_age_floor) = self.ttl_cutoffs_secs();
        let rows = sqlx::query(
            "SELECT goal_id, session_id, model, workspace, \
                    created_at, updated_at, last_active_at \
             FROM claude_session_bindings \
             WHERE last_session_invalid = 0 \
               AND (?1 IS NULL OR last_active_at >= ?1) \
               AND (?2 IS NULL OR created_at      >= ?2) \
             ORDER BY last_active_at DESC",
        )
        .bind(idle_floor)
        .bind(max_age_floor)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(row_to_binding(r)?);
        }
        Ok(out)
    }
}

// `Path` import is used by future helpers; keep the dep silenced.
const _: fn() = || {
    let _: &Path = Path::new("/");
};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_is_idempotent_in_memory() {
        // First open creates schema; second open over the same pool
        // file path must succeed without error.
        let store = SqliteBindingStore::open_memory().await.unwrap();
        // Re-running migrate on the same pool must also be fine.
        SqliteBindingStore::migrate(store.pool_for_test())
            .await
            .unwrap();
    }
}
