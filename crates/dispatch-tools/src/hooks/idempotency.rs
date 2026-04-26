//! Phase 67.F.3 — SQLite-backed idempotency log for hook firings.
//!
//! Why: NATS is at-least-once, the daemon may restart between
//! "decide hook fires" and "hook side effect succeeded", and a
//! `dispatch_phase` hook firing twice would spawn two redundant
//! goals. We persist a `(goal_id, transition, action_kind)` tuple
//! atomically with the side effect; the next attempt sees the row
//! and skips.
//!
//! Schema:
//!
//! ```sql
//! CREATE TABLE hook_dispatched (
//!   goal_id     TEXT NOT NULL,
//!   transition  TEXT NOT NULL,
//!   action_kind TEXT NOT NULL,
//!   action_id   TEXT NOT NULL DEFAULT '',
//!   ts          INTEGER NOT NULL,
//!   PRIMARY KEY (goal_id, transition, action_kind, action_id)
//! );
//! ```
//!
//! `action_id` discriminator allows multiple hooks of the same kind
//! to coexist (two `notify_channel`s to different recipients) by
//! letting the caller stamp each with a stable id (typically the
//! `CompletionHook.id` assigned at registration).

use chrono::Utc;
use nexo_driver_types::GoalId;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use thiserror::Error;

use super::types::{HookAction, HookTransition};

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Error)]
pub enum IdempotencyError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct HookIdempotencyStore {
    pool: SqlitePool,
}

impl HookIdempotencyStore {
    pub async fn open(path: &str) -> Result<Self, IdempotencyError> {
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

    pub async fn open_memory() -> Result<Self, IdempotencyError> {
        Self::open(":memory:").await
    }

    async fn migrate(pool: &SqlitePool) -> Result<(), IdempotencyError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS hook_dispatched (\
                goal_id     TEXT NOT NULL,\
                transition  TEXT NOT NULL,\
                action_kind TEXT NOT NULL,\
                action_id   TEXT NOT NULL DEFAULT '',\
                ts          INTEGER NOT NULL,\
                PRIMARY KEY (goal_id, transition, action_kind, action_id)\
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Atomically claim a `(goal_id, transition, action_kind, action_id)`
    /// slot. Returns `true` when the row was inserted (this caller
    /// is first), `false` when a prior firing already claimed it.
    pub async fn try_claim(
        &self,
        goal_id: GoalId,
        transition: HookTransition,
        action: &HookAction,
        action_id: &str,
    ) -> Result<bool, IdempotencyError> {
        let kind = action_kind_str(action);
        let now = Utc::now().timestamp();
        let res = sqlx::query(
            "INSERT OR IGNORE INTO hook_dispatched \
                (goal_id, transition, action_kind, action_id, ts) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(goal_id.0.to_string())
        .bind(transition.as_str())
        .bind(kind)
        .bind(action_id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Pure read — has this slot already been claimed?
    pub async fn was_dispatched(
        &self,
        goal_id: GoalId,
        transition: HookTransition,
        action: &HookAction,
        action_id: &str,
    ) -> Result<bool, IdempotencyError> {
        let kind = action_kind_str(action);
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM hook_dispatched \
             WHERE goal_id = ?1 AND transition = ?2 AND action_kind = ?3 AND action_id = ?4 \
             LIMIT 1",
        )
        .bind(goal_id.0.to_string())
        .bind(transition.as_str())
        .bind(kind)
        .bind(action_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// B10 — release a claim made by `try_claim` when the action
    /// itself failed. Without this, a transient adapter / network
    /// failure leaves the slot taken forever and any retry fails
    /// with `AlreadyDispatched`. Idempotent: deleting a row that
    /// was never inserted is a no-op.
    pub async fn release_claim(
        &self,
        goal_id: GoalId,
        transition: HookTransition,
        action: &HookAction,
        action_id: &str,
    ) -> Result<(), IdempotencyError> {
        let kind = action_kind_str(action);
        sqlx::query(
            "DELETE FROM hook_dispatched \
             WHERE goal_id = ?1 AND transition = ?2 \
               AND action_kind = ?3 AND action_id = ?4",
        )
        .bind(goal_id.0.to_string())
        .bind(transition.as_str())
        .bind(kind)
        .bind(action_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop every row tied to a goal — used by `evict_completed` /
    /// goal removal so the table doesn't grow unbounded.
    pub async fn forget_goal(&self, goal_id: GoalId) -> Result<u64, IdempotencyError> {
        let res = sqlx::query("DELETE FROM hook_dispatched WHERE goal_id = ?1")
            .bind(goal_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }
}

fn action_kind_str(a: &HookAction) -> &'static str {
    match a {
        HookAction::NotifyOrigin => "notify_origin",
        HookAction::NotifyChannel { .. } => "notify_channel",
        HookAction::DispatchPhase { .. } => "dispatch_phase",
        HookAction::DispatchAudit { .. } => "dispatch_audit",
        HookAction::NatsPublish { .. } => "nats_publish",
        HookAction::Shell { .. } => "shell",
    }
}
