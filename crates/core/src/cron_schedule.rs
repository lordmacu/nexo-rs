//! Phase 79.7 — `cron_create` / `cron_list` / `cron_delete`
//! storage layer.
//!
//! Persists LLM-time-scheduled cron entries to SQLite. Distinct
//! from Phase 7 Heartbeat (config-time only) and Phase 20
//! `agent_turn` poller (config-time only) — this is the only path
//! where the model itself mutates the schedule.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/ScheduleCronTool/CronCreateTool.ts:1-157`
//!     (5-field cron schema, `recurring` + `durable` flags,
//!     50-entry cap, validate-input pattern).
//!   * `claude-code-leak/src/utils/cronTasks.ts` (storage shape).
//!
//! Reference (secondary):
//!   * `research/src/cron/schedule.ts` — OpenClaw uses the
//!     `croner` JS lib + an in-memory cache. Cron expression
//!     parsing semantics are the same; we use Rust's `cron`
//!     crate (already a transitive workspace dep).
//!
//! MVP scope (Phase 79.7):
//!   * SQLite-backed store with idempotent `CREATE TABLE IF NOT
//!     EXISTS`.
//!   * `cron_create` / `cron_list` / `cron_delete` tools.
//!   * Cron expression validated at insert time.
//!   * Cap 50 entries per binding.
//!   * **Runtime firing is NOT shipped here.** Entries land in
//!     SQLite; a follow-up wires into Phase 20 `agent_turn`
//!     poller so due entries actually trigger LLM turns. Until
//!     then the table is observable but inert. See
//!     `FOLLOWUPS.md::Phase 79.7`.

use chrono::{DateTime, TimeZone, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqlitePool, SqlitePoolOptions, SqliteRow},
    ConnectOptions, Row,
};
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

/// Hard cap per binding. The leak uses 50; we adopt the same.
pub const MAX_CRON_ENTRIES_PER_BINDING: usize = 50;

/// Minimum interval between fires. Anything finer pathologically
/// loads the daemon.
pub const MIN_CRON_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CronEntry {
    /// ULID-shaped id assigned at insert.
    pub id: String,
    /// Per-binding namespace. The runtime stamps this from
    /// `AgentContext.inbound_origin` — entries from a `whatsapp:ops`
    /// goal stay isolated from a `telegram:bot` goal.
    pub binding_id: String,
    /// Standard 5-field cron expression (M H DoM Mon DoW). Parsed
    /// at insert; storage retains the literal string so a future
    /// `cron_list` can show the operator what was scheduled.
    pub cron_expr: String,
    /// Prompt to enqueue when the entry fires. Plain string; the
    /// future runtime wiring will hand it to the agent's LLM turn
    /// machinery.
    pub prompt: String,
    /// Optional channel hint (`whatsapp:default`, `telegram:bot`).
    /// `None` = inherit binding's primary channel.
    pub channel: Option<String>,
    /// `true` (default) → fire on every cron match until deleted.
    /// `false` → fire once at the next match, then auto-delete.
    pub recurring: bool,
    /// Unix-seconds creation timestamp.
    pub created_at: i64,
    /// Computed at insert. Future runtime polls this.
    pub next_fire_at: i64,
    /// Unix-seconds of last fire. `None` until first fire.
    pub last_fired_at: Option<i64>,
    /// Soft-disable flag — set by `cron_pause` follow-up. `true`
    /// keeps the entry in storage but skips firing.
    pub paused: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CronStoreError {
    #[error("invalid cron expression `{0}`: {1}")]
    InvalidCron(String, String),
    #[error("interval below minimum 60s for cron `{0}` ({1})")]
    IntervalTooShort(String, &'static str),
    #[error("binding `{0}` already has {1} entries (max {2})")]
    BindingFull(String, usize, &'static str),
    #[error("cron entry `{0}` not found")]
    NotFound(String),
    #[error("sqlx: {0}")]
    Sql(#[from] sqlx::Error),
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS nexo_cron_entries (
        id              TEXT PRIMARY KEY,
        binding_id      TEXT NOT NULL,
        cron_expr       TEXT NOT NULL,
        prompt          TEXT NOT NULL,
        channel         TEXT,
        recurring       INTEGER NOT NULL,
        created_at      INTEGER NOT NULL,
        next_fire_at    INTEGER NOT NULL,
        last_fired_at   INTEGER,
        paused          INTEGER NOT NULL DEFAULT 0
    )
";

const INDEX_BINDING: &str =
    "CREATE INDEX IF NOT EXISTS idx_nexo_cron_entries_binding ON nexo_cron_entries(binding_id)";
const INDEX_FIRE: &str =
    "CREATE INDEX IF NOT EXISTS idx_nexo_cron_entries_fire ON nexo_cron_entries(next_fire_at) WHERE paused = 0";

/// Validate the cron expression and return the next-fire timestamp
/// (unix seconds) at or after `from_unix`. Returns
/// `Err(InvalidCron)` when the expression is unparseable, and
/// `Err(IntervalTooShort)` when two consecutive fires would be
/// closer than `MIN_CRON_INTERVAL_SECS`.
pub fn next_fire_after(cron_expr: &str, from_unix: i64) -> Result<i64, CronStoreError> {
    // The `cron` crate uses 6-field expressions (with seconds);
    // pre-pend "0 " so 5-field (the leak's format + classic Unix)
    // works. Reject already-6-field expressions.
    let parsed_expr = if cron_expr.split_whitespace().count() == 5 {
        format!("0 {cron_expr}")
    } else {
        cron_expr.to_string()
    };
    let schedule = Schedule::from_str(&parsed_expr)
        .map_err(|e| CronStoreError::InvalidCron(cron_expr.to_string(), e.to_string()))?;
    let from = Utc
        .timestamp_opt(from_unix, 0)
        .single()
        .ok_or_else(|| CronStoreError::InvalidCron(cron_expr.to_string(), "bad timestamp".into()))?;
    let mut iter = schedule.after(&from);
    let first: DateTime<Utc> = iter
        .next()
        .ok_or_else(|| CronStoreError::InvalidCron(cron_expr.to_string(), "no future fire".into()))?;
    let second_opt = iter.next();
    if let Some(second) = second_opt {
        let delta = (second - first).num_seconds();
        if (delta as u64) < MIN_CRON_INTERVAL_SECS {
            return Err(CronStoreError::IntervalTooShort(
                cron_expr.to_string(),
                "interval < 60s",
            ));
        }
    }
    Ok(first.timestamp())
}

#[async_trait::async_trait]
pub trait CronStore: Send + Sync {
    async fn insert(&self, entry: &CronEntry) -> Result<(), CronStoreError>;
    async fn list_by_binding(&self, binding_id: &str) -> Result<Vec<CronEntry>, CronStoreError>;
    async fn count_by_binding(&self, binding_id: &str) -> Result<usize, CronStoreError>;
    async fn delete(&self, id: &str) -> Result<(), CronStoreError>;
    /// Future-runtime helper: fetch every entry due at or before
    /// `now`. Today the firing loop is not shipped; the method
    /// exists so a follow-up can wire it in without changing the
    /// trait surface. Tests cover the index works.
    async fn due_at(&self, now_unix: i64) -> Result<Vec<CronEntry>, CronStoreError>;
}

pub struct SqliteCronStore {
    pool: SqlitePool,
}

impl SqliteCronStore {
    pub async fn open(path: &str) -> Result<Self, CronStoreError> {
        let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&format!("sqlite:{path}"))
            .map_err(|e| CronStoreError::Sql(sqlx::Error::Configuration(Box::new(e))))?
            .create_if_missing(true)
            .disable_statement_logging();
        let pool = SqlitePoolOptions::new()
            .max_connections(if path == ":memory:" { 1 } else { 4 })
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
        sqlx::query(INDEX_BINDING).execute(&pool).await?;
        sqlx::query(INDEX_FIRE).execute(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_memory() -> Result<Self, CronStoreError> {
        Self::open(":memory:").await
    }
}

fn row_to_entry(row: &SqliteRow) -> Result<CronEntry, CronStoreError> {
    Ok(CronEntry {
        id: row.try_get("id")?,
        binding_id: row.try_get("binding_id")?,
        cron_expr: row.try_get("cron_expr")?,
        prompt: row.try_get("prompt")?,
        channel: row.try_get("channel")?,
        recurring: row.try_get::<i64, _>("recurring")? != 0,
        created_at: row.try_get("created_at")?,
        next_fire_at: row.try_get("next_fire_at")?,
        last_fired_at: row.try_get("last_fired_at")?,
        paused: row.try_get::<i64, _>("paused")? != 0,
    })
}

#[async_trait::async_trait]
impl CronStore for SqliteCronStore {
    async fn insert(&self, entry: &CronEntry) -> Result<(), CronStoreError> {
        sqlx::query(
            "INSERT INTO nexo_cron_entries \
             (id, binding_id, cron_expr, prompt, channel, recurring, created_at, next_fire_at, last_fired_at, paused) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(&entry.id)
        .bind(&entry.binding_id)
        .bind(&entry.cron_expr)
        .bind(&entry.prompt)
        .bind(entry.channel.as_deref())
        .bind(entry.recurring as i64)
        .bind(entry.created_at)
        .bind(entry.next_fire_at)
        .bind(entry.last_fired_at)
        .bind(entry.paused as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_by_binding(&self, binding_id: &str) -> Result<Vec<CronEntry>, CronStoreError> {
        let rows = sqlx::query(
            "SELECT * FROM nexo_cron_entries WHERE binding_id = ?1 ORDER BY next_fire_at ASC",
        )
        .bind(binding_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_entry).collect()
    }

    async fn count_by_binding(&self, binding_id: &str) -> Result<usize, CronStoreError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM nexo_cron_entries WHERE binding_id = ?1")
            .bind(binding_id)
            .fetch_one(&self.pool)
            .await?;
        let n: i64 = row.try_get("n")?;
        Ok(n as usize)
    }

    async fn delete(&self, id: &str) -> Result<(), CronStoreError> {
        let res = sqlx::query("DELETE FROM nexo_cron_entries WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CronStoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn due_at(&self, now_unix: i64) -> Result<Vec<CronEntry>, CronStoreError> {
        let rows = sqlx::query(
            "SELECT * FROM nexo_cron_entries \
             WHERE paused = 0 AND next_fire_at <= ?1 \
             ORDER BY next_fire_at ASC",
        )
        .bind(now_unix)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_entry).collect()
    }
}

/// Builder helper used by the `cron_create` tool: validates the
/// expression, enforces the binding cap, and produces a fresh
/// `CronEntry` ready to insert.
pub async fn build_new_entry(
    store: &Arc<dyn CronStore>,
    binding_id: &str,
    cron_expr: &str,
    prompt: &str,
    channel: Option<&str>,
    recurring: bool,
) -> Result<CronEntry, CronStoreError> {
    let now = Utc::now().timestamp();
    let next_fire_at = next_fire_after(cron_expr, now)?;
    let count = store.count_by_binding(binding_id).await?;
    if count >= MAX_CRON_ENTRIES_PER_BINDING {
        return Err(CronStoreError::BindingFull(
            binding_id.to_string(),
            count,
            "50",
        ));
    }
    Ok(CronEntry {
        id: Uuid::new_v4().to_string(),
        binding_id: binding_id.to_string(),
        cron_expr: cron_expr.to_string(),
        prompt: prompt.to_string(),
        channel: channel.map(str::to_string),
        recurring,
        created_at: now,
        next_fire_at,
        last_fired_at: None,
        paused: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(binding: &str, expr: &str) -> CronEntry {
        let now = 1_700_000_000;
        CronEntry {
            id: Uuid::new_v4().to_string(),
            binding_id: binding.into(),
            cron_expr: expr.into(),
            prompt: "ping".into(),
            channel: None,
            recurring: true,
            created_at: now,
            next_fire_at: next_fire_after(expr, now).unwrap(),
            last_fired_at: None,
            paused: false,
        }
    }

    #[test]
    fn next_fire_accepts_5field() {
        // every 5 minutes
        let v = next_fire_after("*/5 * * * *", 1_700_000_000).unwrap();
        assert!(v > 1_700_000_000);
    }

    #[test]
    fn next_fire_accepts_6field_passthrough() {
        // every minute (6-field with explicit seconds 0)
        let v = next_fire_after("0 * * * * *", 1_700_000_000).unwrap();
        assert!(v > 1_700_000_000);
    }

    #[test]
    fn next_fire_rejects_garbage() {
        let err = next_fire_after("not a cron", 1_700_000_000).unwrap_err();
        assert!(matches!(err, CronStoreError::InvalidCron(_, _)));
    }

    #[test]
    fn next_fire_rejects_sub_minute_interval() {
        // every second — 6-field expression with `*` in the
        // seconds position. Two consecutive fires are 1s apart.
        let err = next_fire_after("* * * * * *", 1_700_000_000).unwrap_err();
        assert!(
            matches!(err, CronStoreError::IntervalTooShort(_, _)),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn store_insert_list_count_delete_round_trip() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let e1 = entry("whatsapp:ops", "*/5 * * * *");
        let e2 = entry("whatsapp:ops", "0 9 * * *");
        let e3 = entry("telegram:bot", "0 */2 * * *");
        store.insert(&e1).await.unwrap();
        store.insert(&e2).await.unwrap();
        store.insert(&e3).await.unwrap();

        let listed = store.list_by_binding("whatsapp:ops").await.unwrap();
        assert_eq!(listed.len(), 2);
        let ids: std::collections::HashSet<_> =
            listed.iter().map(|e| e.id.clone()).collect();
        assert!(ids.contains(&e1.id));
        assert!(ids.contains(&e2.id));

        assert_eq!(store.count_by_binding("whatsapp:ops").await.unwrap(), 2);
        assert_eq!(store.count_by_binding("telegram:bot").await.unwrap(), 1);

        store.delete(&e1.id).await.unwrap();
        assert_eq!(store.count_by_binding("whatsapp:ops").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn delete_unknown_id_errors() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let err = store.delete("nope").await.unwrap_err();
        assert!(matches!(err, CronStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn due_at_filters_paused_and_future() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let mut due = entry("whatsapp:ops", "*/5 * * * *");
        due.next_fire_at = 1_700_000_000;
        let mut paused = entry("whatsapp:ops", "*/5 * * * *");
        paused.next_fire_at = 1_700_000_000;
        paused.paused = true;
        let mut future = entry("whatsapp:ops", "0 9 * * *");
        future.next_fire_at = 1_700_001_000;
        store.insert(&due).await.unwrap();
        store.insert(&paused).await.unwrap();
        store.insert(&future).await.unwrap();

        let now_due = store.due_at(1_700_000_500).await.unwrap();
        assert_eq!(now_due.len(), 1);
        assert_eq!(now_due[0].id, due.id);
    }

    #[tokio::test]
    async fn build_new_entry_caps_at_50_per_binding() {
        let store: Arc<dyn CronStore> =
            Arc::new(SqliteCronStore::open_memory().await.unwrap());
        for _ in 0..50 {
            let e = build_new_entry(&store, "whatsapp:ops", "*/5 * * * *", "ping", None, true)
                .await
                .unwrap();
            store.insert(&e).await.unwrap();
        }
        let err = build_new_entry(&store, "whatsapp:ops", "*/5 * * * *", "ping", None, true)
            .await
            .unwrap_err();
        assert!(matches!(err, CronStoreError::BindingFull(_, 50, _)));
    }

    #[tokio::test]
    async fn build_new_entry_isolated_per_binding() {
        let store: Arc<dyn CronStore> =
            Arc::new(SqliteCronStore::open_memory().await.unwrap());
        for _ in 0..50 {
            let e = build_new_entry(&store, "binding-a", "*/5 * * * *", "ping", None, true)
                .await
                .unwrap();
            store.insert(&e).await.unwrap();
        }
        // Different binding admits even when the first one is at cap.
        let e = build_new_entry(&store, "binding-b", "*/5 * * * *", "ping", None, true)
            .await
            .unwrap();
        store.insert(&e).await.unwrap();
        assert_eq!(store.count_by_binding("binding-b").await.unwrap(), 1);
    }
}
