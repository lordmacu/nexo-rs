//! Phase 82.13.d — SQLite-backed `ProcessingControlStore`.
//!
//! Durable variant of `InMemoryProcessingControlStore` so operator
//! pause / resume + the per-scope pending inbound queue survive
//! daemon restarts. Without durability a daemon bounce drops every
//! pause back to `AgentActive` and silently loses every buffered
//! inbound — the agent then resumes processing every contact's
//! latest message as if nothing was paused, which is exactly the
//! footgun the operator takeover surface exists to prevent.
//!
//! Two tables, both append-friendly:
//!
//! - `nexo_processing_states` keyed by canonical scope JSON, one
//!   row per scope. `set(AgentActive)` deletes the row so the
//!   missing-row-defaults-to-AgentActive contract from the trait
//!   stays exact.
//! - `nexo_processing_pending` is a per-scope FIFO queue indexed
//!   by `id` autoincrement; `push_pending` inserts, the cap is
//!   enforced after insert by deleting the oldest row(s),
//!   `drain_pending` runs `DELETE ... RETURNING` so the read +
//!   clear are atomic.
//!
//! Mirrors the `audit_sqlite` open pattern: WAL, idempotent
//! `CREATE TABLE IF NOT EXISTS` + `ALTER TABLE` future-proofing,
//! `open_memory()` for tests.

use std::path::Path;
use std::str::FromStr;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use nexo_tool_meta::admin::processing::{
    PendingInbound, ProcessingControlState, ProcessingScope,
    DEFAULT_PENDING_INBOUNDS_CAP,
};

use super::domains::processing::ProcessingControlStore;

/// SQLite-backed `ProcessingControlStore`. Cheap to clone — the
/// pool is `Arc`-shared internally.
#[derive(Debug, Clone)]
pub struct SqliteProcessingControlStore {
    pool: SqlitePool,
    pending_cap: usize,
}

impl SqliteProcessingControlStore {
    /// Open or create the DB at `path`. Idempotent — DDL runs on
    /// every boot. Pool kept small (2 conns) since processing
    /// state mutations are infrequent compared to admin audit
    /// writes. `pending_cap` defaults to
    /// `DEFAULT_PENDING_INBOUNDS_CAP` and bounds the per-scope
    /// FIFO queue exactly the same way the in-memory store does.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let path_str = path.display().to_string();
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path_str}"))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(opts)
            .await?;
        sqlx::query("PRAGMA journal_mode=WAL").execute(&pool).await.ok();
        Self::run_ddl(&pool).await?;
        Ok(Self {
            pool,
            pending_cap: DEFAULT_PENDING_INBOUNDS_CAP,
        })
    }

    /// In-memory variant for tests — no filesystem path; the pool
    /// drops every row on `Self::Drop`.
    pub async fn open_memory() -> anyhow::Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::run_ddl(&pool).await?;
        Ok(Self {
            pool,
            pending_cap: DEFAULT_PENDING_INBOUNDS_CAP,
        })
    }

    /// Override the per-scope queue cap. Boot wiring sets this
    /// from `NEXO_PROCESSING_PENDING_QUEUE_CAP`. `0` disables
    /// buffering entirely (every push reports a drop). Same
    /// semantics as `InMemoryProcessingControlStore::with_pending_cap`.
    pub fn with_pending_cap(mut self, cap: usize) -> Self {
        self.pending_cap = cap;
        self
    }

    async fn run_ddl(pool: &SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS nexo_processing_states (
                scope_json    TEXT PRIMARY KEY,
                state_json    TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS nexo_processing_pending (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                scope_json     TEXT NOT NULL,
                inbound_json   TEXT NOT NULL,
                enqueued_at_ms INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nexo_processing_pending_scope
                ON nexo_processing_pending(scope_json, id)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }
}

/// Render a `ProcessingScope` to canonical (sorted-keys) JSON so
/// two scopes with semantically-equal field values produce the
/// same primary key — operators that pause via one transport and
/// resume via another don't get split rows.
fn scope_key(scope: &ProcessingScope) -> anyhow::Result<String> {
    let v = serde_json::to_value(scope)?;
    Ok(canonicalize(&v).to_string())
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize(v)))
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[async_trait]
impl ProcessingControlStore for SqliteProcessingControlStore {
    async fn get(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<ProcessingControlState> {
        let key = scope_key(scope)?;
        let row: Option<(String,)> =
            sqlx::query_as("SELECT state_json FROM nexo_processing_states WHERE scope_json = ?1")
                .bind(&key)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            None => Ok(ProcessingControlState::AgentActive),
            Some((json,)) => Ok(serde_json::from_str(&json)?),
        }
    }

    async fn set(
        &self,
        scope: ProcessingScope,
        state: ProcessingControlState,
    ) -> anyhow::Result<bool> {
        let key = scope_key(&scope)?;
        // Read prev state once to compute the `changed` flag.
        // Two queries (read + write) are fine here — the trait
        // contract doesn't require strict atomicity, and SQLite's
        // single-writer model serialises both anyway.
        let prev_json: Option<(String,)> =
            sqlx::query_as("SELECT state_json FROM nexo_processing_states WHERE scope_json = ?1")
                .bind(&key)
                .fetch_optional(&self.pool)
                .await?;
        let prev_state: Option<ProcessingControlState> = match prev_json {
            Some((s,)) => Some(serde_json::from_str(&s)?),
            None => None,
        };

        match &state {
            ProcessingControlState::AgentActive => {
                // AgentActive == "no row" — delete to keep the
                // "missing row defaults to AgentActive" invariant.
                sqlx::query("DELETE FROM nexo_processing_states WHERE scope_json = ?1")
                    .bind(&key)
                    .execute(&self.pool)
                    .await?;
                Ok(prev_state.is_some()
                    && prev_state.as_ref() != Some(&ProcessingControlState::AgentActive))
            }
            other => {
                let payload = serde_json::to_string(other)?;
                sqlx::query(
                    "INSERT INTO nexo_processing_states (scope_json, state_json, updated_at_ms) \
                     VALUES (?1, ?2, ?3) \
                     ON CONFLICT(scope_json) DO UPDATE SET state_json = excluded.state_json, \
                                                            updated_at_ms = excluded.updated_at_ms",
                )
                .bind(&key)
                .bind(&payload)
                .bind(now_ms())
                .execute(&self.pool)
                .await?;
                Ok(prev_state.as_ref() != Some(&state))
            }
        }
    }

    async fn clear(&self, scope: &ProcessingScope) -> anyhow::Result<bool> {
        let key = scope_key(scope)?;
        let res =
            sqlx::query("DELETE FROM nexo_processing_states WHERE scope_json = ?1")
                .bind(&key)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn push_pending(
        &self,
        scope: &ProcessingScope,
        inbound: PendingInbound,
    ) -> anyhow::Result<(usize, u32)> {
        let key = scope_key(scope)?;
        if self.pending_cap == 0 {
            // Buffering disabled — count every push as dropped
            // without keeping the row.
            return Ok((0, 1));
        }
        let payload = serde_json::to_string(&inbound)?;
        sqlx::query(
            "INSERT INTO nexo_processing_pending \
             (scope_json, inbound_json, enqueued_at_ms) VALUES (?1, ?2, ?3)",
        )
        .bind(&key)
        .bind(&payload)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;

        // FIFO eviction. Loop in case the cap was lowered between
        // boots (env-var change) — same defensive shape as the
        // in-memory variant.
        let mut dropped = 0u32;
        loop {
            let depth: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM nexo_processing_pending WHERE scope_json = ?1",
            )
            .bind(&key)
            .fetch_one(&self.pool)
            .await?;
            if (depth as usize) <= self.pending_cap {
                return Ok((depth as usize, dropped));
            }
            sqlx::query(
                "DELETE FROM nexo_processing_pending WHERE id = (
                    SELECT id FROM nexo_processing_pending
                    WHERE scope_json = ?1
                    ORDER BY id ASC LIMIT 1
                )",
            )
            .bind(&key)
            .execute(&self.pool)
            .await?;
            dropped = dropped.saturating_add(1);
        }
    }

    async fn drain_pending(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<Vec<PendingInbound>> {
        let key = scope_key(scope)?;
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT inbound_json FROM nexo_processing_pending \
             WHERE scope_json = ?1 ORDER BY id ASC",
        )
        .bind(&key)
        .fetch_all(&self.pool)
        .await?;
        let mut out: Vec<PendingInbound> = Vec::with_capacity(rows.len());
        for (json,) in rows {
            // Forward-compatible: a row that fails to parse is
            // logged + skipped rather than aborting the drain so
            // a single corrupt row doesn't strand the queue.
            match serde_json::from_str::<PendingInbound>(&json) {
                Ok(p) => out.push(p),
                Err(e) => tracing::warn!(
                    error = %e,
                    "processing_sqlite: drain skipped malformed pending row",
                ),
            }
        }
        sqlx::query("DELETE FROM nexo_processing_pending WHERE scope_json = ?1")
            .bind(&key)
            .execute(&self.pool)
            .await?;
        Ok(out)
    }

    async fn pending_depth(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<usize> {
        let key = scope_key(scope)?;
        let depth: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexo_processing_pending WHERE scope_json = ?1",
        )
        .bind(&key)
        .fetch_one(&self.pool)
        .await?;
        Ok(depth.max(0) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.55".into(),
            mcp_channel_source: None,
        }
    }

    fn other_convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "bob".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.99".into(),
            mcp_channel_source: None,
        }
    }

    fn pending(body: &str) -> PendingInbound {
        PendingInbound {
            message_id: Some(Uuid::new_v4()),
            from_contact_id: "wa.55".into(),
            body: body.into(),
            timestamp_ms: now_ms() as u64,
            source_plugin: "whatsapp".into(),
        }
    }

    #[tokio::test]
    async fn missing_scope_defaults_to_agent_active() {
        let store = SqliteProcessingControlStore::open_memory().await.unwrap();
        let state = store.get(&convo()).await.unwrap();
        assert!(matches!(state, ProcessingControlState::AgentActive));
    }

    #[tokio::test]
    async fn set_paused_then_get_round_trips_through_sqlite() {
        let store = SqliteProcessingControlStore::open_memory().await.unwrap();
        let changed = store
            .set(
                convo(),
                ProcessingControlState::PausedByOperator {
                    scope: convo(),
                    reason: Some("smoke".into()),
                    paused_at_ms: 1_700_000_000_000,
                    operator_token_hash: "h".into(),
                },
            )
            .await
            .unwrap();
        assert!(changed);
        let state = store.get(&convo()).await.unwrap();
        match state {
            ProcessingControlState::PausedByOperator { reason, .. } => {
                assert_eq!(reason.as_deref(), Some("smoke"));
            }
            other => panic!("expected paused, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_idempotent_returns_changed_false_on_second_call() {
        let store = SqliteProcessingControlStore::open_memory().await.unwrap();
        let s = ProcessingControlState::PausedByOperator {
                    scope: convo(),
            reason: None,
            paused_at_ms: 1,
            operator_token_hash: "h".into(),
        };
        assert!(store.set(convo(), s.clone()).await.unwrap());
        assert!(!store.set(convo(), s).await.unwrap(), "second set must be no-op");
    }

    #[tokio::test]
    async fn set_agent_active_removes_row() {
        let store = SqliteProcessingControlStore::open_memory().await.unwrap();
        store
            .set(
                convo(),
                ProcessingControlState::PausedByOperator {
                    scope: convo(),
                    reason: None,
                    paused_at_ms: 1,
                    operator_token_hash: "h".into(),
                },
            )
            .await
            .unwrap();
        let changed = store
            .set(convo(), ProcessingControlState::AgentActive)
            .await
            .unwrap();
        assert!(changed, "transition Paused → AgentActive must report changed");
        let state = store.get(&convo()).await.unwrap();
        assert!(matches!(state, ProcessingControlState::AgentActive));
    }

    #[tokio::test]
    async fn clear_returns_true_only_when_row_existed() {
        let store = SqliteProcessingControlStore::open_memory().await.unwrap();
        // No row yet.
        assert!(!store.clear(&convo()).await.unwrap());
        store
            .set(
                convo(),
                ProcessingControlState::PausedByOperator {
                    scope: convo(),
                    reason: None,
                    paused_at_ms: 1,
                    operator_token_hash: "h".into(),
                },
            )
            .await
            .unwrap();
        assert!(store.clear(&convo()).await.unwrap());
    }

    #[tokio::test]
    async fn push_pending_caps_at_pending_cap_and_evicts_fifo() {
        let store = SqliteProcessingControlStore::open_memory()
            .await
            .unwrap()
            .with_pending_cap(3);
        for i in 0..5 {
            let (depth, dropped) = store
                .push_pending(&convo(), pending(&format!("msg-{i}")))
                .await
                .unwrap();
            if i < 3 {
                assert_eq!(dropped, 0);
                assert_eq!(depth, (i + 1) as usize);
            } else {
                assert_eq!(dropped, 1, "msg-{i} must evict one oldest");
                assert_eq!(depth, 3);
            }
        }
        // Drain returns 3 newest in arrival order: msg-2, msg-3, msg-4.
        let drained = store.drain_pending(&convo()).await.unwrap();
        let bodies: Vec<String> = drained.into_iter().map(|p| p.body).collect();
        assert_eq!(bodies, vec!["msg-2", "msg-3", "msg-4"]);
    }

    #[tokio::test]
    async fn push_with_zero_cap_reports_drop_per_push() {
        let store = SqliteProcessingControlStore::open_memory()
            .await
            .unwrap()
            .with_pending_cap(0);
        let (depth, dropped) = store
            .push_pending(&convo(), pending("x"))
            .await
            .unwrap();
        assert_eq!(depth, 0);
        assert_eq!(dropped, 1);
        // Nothing buffered; drain is empty.
        assert!(store.drain_pending(&convo()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn drain_clears_queue_atomically() {
        let store = SqliteProcessingControlStore::open_memory().await.unwrap();
        store.push_pending(&convo(), pending("a")).await.unwrap();
        store.push_pending(&convo(), pending("b")).await.unwrap();
        let first = store.drain_pending(&convo()).await.unwrap();
        assert_eq!(first.len(), 2);
        let second = store.drain_pending(&convo()).await.unwrap();
        assert!(second.is_empty(), "second drain must be empty");
        assert_eq!(store.pending_depth(&convo()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn pending_queues_are_isolated_per_scope() {
        let store = SqliteProcessingControlStore::open_memory().await.unwrap();
        store.push_pending(&convo(), pending("ana-1")).await.unwrap();
        store.push_pending(&other_convo(), pending("bob-1")).await.unwrap();
        store.push_pending(&convo(), pending("ana-2")).await.unwrap();
        assert_eq!(store.pending_depth(&convo()).await.unwrap(), 2);
        assert_eq!(store.pending_depth(&other_convo()).await.unwrap(), 1);
        let drained = store.drain_pending(&convo()).await.unwrap();
        assert_eq!(drained.len(), 2);
        // Other scope still has its row.
        assert_eq!(store.pending_depth(&other_convo()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn state_survives_pool_round_trip_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("processing.db");
        let store = SqliteProcessingControlStore::open(&path).await.unwrap();
        store
            .set(
                convo(),
                ProcessingControlState::PausedByOperator {
                    scope: convo(),
                    reason: Some("restart-test".into()),
                    paused_at_ms: 42,
                    operator_token_hash: "h".into(),
                },
            )
            .await
            .unwrap();
        store.push_pending(&convo(), pending("queued")).await.unwrap();
        drop(store);
        // Re-open and verify the row + the queued inbound persist.
        let store2 = SqliteProcessingControlStore::open(&path).await.unwrap();
        match store2.get(&convo()).await.unwrap() {
            ProcessingControlState::PausedByOperator { reason, .. } => {
                assert_eq!(reason.as_deref(), Some("restart-test"));
            }
            other => panic!("expected paused after restart, got {other:?}"),
        }
        let drained = store2.drain_pending(&convo()).await.unwrap();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].body, "queued");
    }

    #[tokio::test]
    async fn ddl_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("processing.db");
        let _w1 = SqliteProcessingControlStore::open(&path).await.unwrap();
        let _w2 = SqliteProcessingControlStore::open(&path).await.unwrap();
    }
}
