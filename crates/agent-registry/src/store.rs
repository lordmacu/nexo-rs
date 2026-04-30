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

use crate::types::{AgentHandle, AgentRunStatus, AgentSleepState};

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
        out.sort_by_key(|b| std::cmp::Reverse(b.started_at));
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
        out.sort_by_key(|b| std::cmp::Reverse(b.started_at));
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
        add_column_if_missing(pool, "plan_mode TEXT").await?;
        add_column_if_missing(pool, "sleep_wake_at INTEGER").await?;
        add_column_if_missing(pool, "sleep_duration_ms INTEGER").await?;
        add_column_if_missing(pool, "sleep_reason TEXT").await?;
        // Phase 80.10 — provenance posture column. Default
        // 'interactive' applies to all pre-80.10 rows during the
        // ALTER pass; new rows write through `kind = ?` in the
        // upsert path. Idempotent: `add_column_if_missing` swallows
        // "duplicate column" errors so re-opening the DB is safe.
        add_column_if_missing(pool, "kind TEXT NOT NULL DEFAULT 'interactive'").await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_agent_registry_kind \
                 ON agent_registry(kind)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_agent_registry_sleep_wake_at \
                 ON agent_registry(sleep_wake_at)",
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

async fn add_column_if_missing(
    pool: &SqlitePool,
    column_sql: &str,
) -> Result<(), AgentRegistryStoreError> {
    let alter = sqlx::query(&format!(
        "ALTER TABLE agent_registry ADD COLUMN {column_sql}"
    ))
    .execute(pool)
    .await;
    if let Err(e) = alter {
        let msg = e.to_string();
        if !msg.contains("duplicate column") {
            return Err(e.into());
        }
    }
    Ok(())
}

fn parse_status(s: &str) -> Result<AgentRunStatus, AgentRegistryStoreError> {
    Ok(match s {
        "running" => AgentRunStatus::Running,
        "sleeping" => AgentRunStatus::Sleeping,
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
    // Phase 79.1 — plan_mode column wins over the handle_json copy so
    // a hot update via `set_plan_mode` is observable without rewriting
    // the whole handle blob.
    let plan_mode: Option<String> = row.try_get("plan_mode").unwrap_or(None);
    handle.plan_mode = plan_mode;
    let sleep_wake_at: Option<i64> = row.try_get("sleep_wake_at").unwrap_or(None);
    let sleep_duration_ms: Option<i64> = row.try_get("sleep_duration_ms").unwrap_or(None);
    let sleep_reason: Option<String> = row.try_get("sleep_reason").unwrap_or(None);
    handle.snapshot.sleep = match (sleep_wake_at, sleep_duration_ms, sleep_reason) {
        (Some(wake_at), Some(duration_ms), Some(reason)) if duration_ms >= 0 => {
            Some(AgentSleepState {
                wake_at: from_unix(wake_at),
                duration_ms: duration_ms as u64,
                reason,
            })
        }
        _ => None,
    };
    // Phase 80.10 — kind column wins over the handle_json copy. Falls
    // back to `Interactive` when the column is missing (rows persisted
    // before the migration ran) or when the JSON blob lacks the field.
    let kind_str: Option<String> = row.try_get("kind").unwrap_or(None);
    if let Some(s) = kind_str {
        handle.kind = crate::types::SessionKind::from_db_str(&s)?;
    }
    Ok(handle)
}

#[async_trait]
impl AgentRegistryStore for SqliteAgentRegistryStore {
    async fn upsert(&self, handle: &AgentHandle) -> Result<(), AgentRegistryStoreError> {
        let json = serde_json::to_string(handle)
            .map_err(|e| AgentRegistryStoreError::Json(e.to_string()))?;
        let sleep = handle.snapshot.sleep.as_ref();
        sqlx::query(
            "INSERT INTO agent_registry (goal_id, phase_id, status, started_at, finished_at, handle_json, plan_mode, sleep_wake_at, sleep_duration_ms, sleep_reason, kind) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(goal_id) DO UPDATE SET \
                 phase_id     = excluded.phase_id, \
                 status       = excluded.status, \
                 finished_at  = excluded.finished_at, \
                 handle_json  = excluded.handle_json, \
                 plan_mode    = excluded.plan_mode, \
                 sleep_wake_at = excluded.sleep_wake_at, \
                 sleep_duration_ms = excluded.sleep_duration_ms, \
                 sleep_reason = excluded.sleep_reason, \
                 kind         = excluded.kind",
        )
        .bind(handle.goal_id.0.to_string())
        .bind(&handle.phase_id)
        .bind(handle.status.as_str())
        .bind(unix(handle.started_at))
        .bind(handle.finished_at.map(unix))
        .bind(json)
        .bind(handle.plan_mode.as_deref())
        .bind(sleep.map(|s| unix(s.wake_at)))
        .bind(sleep.map(|s| s.duration_ms as i64))
        .bind(sleep.map(|s| s.reason.as_str()))
        .bind(handle.kind.as_db_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, goal_id: GoalId) -> Result<Option<AgentHandle>, AgentRegistryStoreError> {
        let row = sqlx::query(
            "SELECT goal_id, phase_id, status, started_at, finished_at, handle_json, plan_mode, sleep_wake_at, sleep_duration_ms, sleep_reason, kind \
             FROM agent_registry WHERE goal_id = ?1 LIMIT 1",
        )
        .bind(goal_id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_handle(&r)).transpose()
    }

    async fn list(&self) -> Result<Vec<AgentHandle>, AgentRegistryStoreError> {
        let rows = sqlx::query(
            "SELECT goal_id, phase_id, status, started_at, finished_at, handle_json, plan_mode, sleep_wake_at, sleep_duration_ms, sleep_reason, kind \
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
            "SELECT goal_id, phase_id, status, started_at, finished_at, handle_json, plan_mode, sleep_wake_at, sleep_duration_ms, sleep_reason, kind \
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

impl SqliteAgentRegistryStore {
    /// Phase 80.10 — list handles filtered by `SessionKind`. Newest
    /// first by `started_at`. Used by `nexo agent ps --kind=...`.
    pub async fn list_by_kind(
        &self,
        kind: crate::types::SessionKind,
    ) -> Result<Vec<AgentHandle>, AgentRegistryStoreError> {
        let rows = sqlx::query(
            "SELECT goal_id, phase_id, status, started_at, finished_at, handle_json, plan_mode, sleep_wake_at, sleep_duration_ms, sleep_reason, kind \
             FROM agent_registry WHERE kind = ?1 ORDER BY started_at DESC",
        )
        .bind(kind.as_db_str())
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(row_to_handle(r)?);
        }
        Ok(out)
    }

    /// Phase 80.10 — kind-aware reattach. Flips `Running` rows to
    /// `LostOnRestart` only for `kind == 'interactive'` — the user is
    /// gone, no caller waiting. Bg / Daemon / DaemonWorker rows keep
    /// `Running` because the operator expects them to survive across
    /// daemon restarts. Returns the count of rows flipped.
    pub async fn reattach_running_kind_aware(
        &self,
    ) -> Result<u64, AgentRegistryStoreError> {
        let now = unix(Utc::now());
        let res = sqlx::query(
            "UPDATE agent_registry \
                SET status = 'lost_on_restart', finished_at = ?1 \
              WHERE status = 'running' AND kind = 'interactive'",
        )
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

// `Path` import is used by future helpers; keep the dep silenced.
const _: fn() = || {
    let _: &Path = Path::new("/");
};

#[cfg(test)]
mod plan_mode_persistence_tests {
    use super::*;
    use crate::types::AgentSnapshot;
    use chrono::Utc;
    use uuid::Uuid;

    fn handle_with_plan_mode(plan_mode: Option<String>) -> AgentHandle {
        AgentHandle {
            goal_id: GoalId(Uuid::new_v4()),
            phase_id: "p1".into(),
            status: AgentRunStatus::Running,
            origin: None,
            dispatcher: None,
            started_at: Utc::now(),
            finished_at: None,
            snapshot: AgentSnapshot::default(),
            plan_mode,
            kind: crate::types::SessionKind::Interactive,
        }
    }

    #[tokio::test]
    async fn migrate_is_idempotent_on_repeated_open() {
        // Memory DB cannot survive a re-open; use a temp file so the
        // second open hits the existing schema and exercises the
        // ALTER-TABLE-tolerant branch.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let _ = SqliteAgentRegistryStore::open(&path).await.unwrap();
        // Second call must not error even though the column already
        // exists.
        let _ = SqliteAgentRegistryStore::open(&path).await.unwrap();
    }

    #[tokio::test]
    async fn upsert_roundtrip_with_plan_mode_some() {
        let store = SqliteAgentRegistryStore::open_memory().await.unwrap();
        let h = handle_with_plan_mode(Some(
            r#"{"state":"on","entered_at":1700000000,"reason":{"kind":"model_requested","reason":"explore"},"prior_mode":"default"}"#.into(),
        ));
        store.upsert(&h).await.unwrap();
        let read = store.get(h.goal_id).await.unwrap().unwrap();
        assert_eq!(read.plan_mode, h.plan_mode);
    }

    #[tokio::test]
    async fn upsert_roundtrip_with_plan_mode_none() {
        let store = SqliteAgentRegistryStore::open_memory().await.unwrap();
        let h = handle_with_plan_mode(None);
        store.upsert(&h).await.unwrap();
        let read = store.get(h.goal_id).await.unwrap().unwrap();
        assert_eq!(read.plan_mode, None);
    }

    #[tokio::test]
    async fn column_overrides_handle_json_on_drift() {
        // Simulate the case where handle_json carries a stale plan_mode
        // (e.g. the column was hot-updated separately). The column
        // wins on read.
        let store = SqliteAgentRegistryStore::open_memory().await.unwrap();
        let mut h = handle_with_plan_mode(Some("\"original\"".into()));
        store.upsert(&h).await.unwrap();
        // Hot-patch only the column.
        sqlx::query("UPDATE agent_registry SET plan_mode = ?1 WHERE goal_id = ?2")
            .bind("\"hotpatched\"")
            .bind(h.goal_id.0.to_string())
            .execute(&store.pool)
            .await
            .unwrap();
        h = store.get(h.goal_id).await.unwrap().unwrap();
        assert_eq!(h.plan_mode.as_deref(), Some("\"hotpatched\""));
    }

    // ── Phase 80.10 — SessionKind ──

    fn handle_with_kind(kind: crate::types::SessionKind) -> AgentHandle {
        let mut h = handle_with_plan_mode(None);
        h.kind = kind;
        h
    }

    #[test]
    fn session_kind_default_is_interactive() {
        assert_eq!(
            crate::types::SessionKind::default(),
            crate::types::SessionKind::Interactive
        );
    }

    #[test]
    fn session_kind_db_round_trip_all_variants() {
        use crate::types::SessionKind;
        for k in [
            SessionKind::Interactive,
            SessionKind::Bg,
            SessionKind::Daemon,
            SessionKind::DaemonWorker,
        ] {
            assert_eq!(SessionKind::from_db_str(k.as_db_str()).unwrap(), k);
        }
    }

    #[test]
    fn session_kind_from_db_str_rejects_unknown() {
        let err = crate::types::SessionKind::from_db_str("garbage").unwrap_err();
        assert!(err.to_string().contains("garbage"));
    }

    #[test]
    fn session_kind_survives_restart_only_for_bg_daemon() {
        use crate::types::SessionKind;
        assert!(!SessionKind::Interactive.survives_restart());
        assert!(SessionKind::Bg.survives_restart());
        assert!(SessionKind::Daemon.survives_restart());
        assert!(SessionKind::DaemonWorker.survives_restart());
    }

    #[test]
    fn agent_handle_serde_default_kind() {
        // Round-trip through serde with the field removed: deserialise
        // must yield Interactive via #[serde(default)].
        let mut h = handle_with_kind(crate::types::SessionKind::Bg);
        let mut json: serde_json::Value =
            serde_json::to_value(&h).unwrap();
        // Strip the `kind` field to simulate a pre-80.10 persisted blob.
        json.as_object_mut().unwrap().remove("kind");
        let parsed: AgentHandle = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.kind, crate::types::SessionKind::Interactive);
        // Sanity: helper handle itself round-trips with kind preserved.
        h.kind = crate::types::SessionKind::Daemon;
        let s = serde_json::to_string(&h).unwrap();
        let back: AgentHandle = serde_json::from_str(&s).unwrap();
        assert_eq!(back.kind, crate::types::SessionKind::Daemon);
    }

    #[tokio::test]
    async fn store_insert_with_kind_round_trips() {
        let store = SqliteAgentRegistryStore::open_memory().await.unwrap();
        let h = handle_with_kind(crate::types::SessionKind::Bg);
        store.upsert(&h).await.unwrap();
        let fetched = store.get(h.goal_id).await.unwrap().unwrap();
        assert_eq!(fetched.kind, crate::types::SessionKind::Bg);
    }

    #[tokio::test]
    async fn list_by_kind_filters_correctly() {
        let store = SqliteAgentRegistryStore::open_memory().await.unwrap();
        store
            .upsert(&handle_with_kind(crate::types::SessionKind::Interactive))
            .await
            .unwrap();
        store
            .upsert(&handle_with_kind(crate::types::SessionKind::Bg))
            .await
            .unwrap();
        store
            .upsert(&handle_with_kind(crate::types::SessionKind::Daemon))
            .await
            .unwrap();
        let bg = store
            .list_by_kind(crate::types::SessionKind::Bg)
            .await
            .unwrap();
        assert_eq!(bg.len(), 1);
        assert_eq!(bg[0].kind, crate::types::SessionKind::Bg);
    }

    #[tokio::test]
    async fn reattach_running_kind_aware_keeps_bg() {
        use crate::types::SessionKind;
        let store = SqliteAgentRegistryStore::open_memory().await.unwrap();
        let bg = handle_with_kind(SessionKind::Bg);
        let interactive = handle_with_kind(SessionKind::Interactive);
        let bg_id = bg.goal_id;
        let inter_id = interactive.goal_id;
        store.upsert(&bg).await.unwrap();
        store.upsert(&interactive).await.unwrap();
        let flipped = store.reattach_running_kind_aware().await.unwrap();
        assert_eq!(flipped, 1, "only the interactive row should flip");
        let bg_after = store.get(bg_id).await.unwrap().unwrap();
        assert_eq!(bg_after.status, AgentRunStatus::Running);
        let inter_after = store.get(inter_id).await.unwrap().unwrap();
        assert_eq!(inter_after.status, AgentRunStatus::LostOnRestart);
        assert!(inter_after.finished_at.is_some());
    }
}
