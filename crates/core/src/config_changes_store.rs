//! Phase 79.10 — durable audit log of `ConfigTool` proposals +
//! their lifecycle (proposed → applied | rolled_back | rejected |
//! expired).
//!
//! Two consumers:
//!   * `crates/core/src/agent/config_tool.rs` writes one row per
//!     state transition (defense-in-depth: even if the staging
//!     file disappears, the audit row survives).
//!   * `crates/core/src/agent/config_changes_tail_tool.rs` is the
//!     read-only LLM tool that lets a model post-mortem its own
//!     mutation history. Same shape as Phase 72 `agent_turns_tail`.
//!
//! Pattern lift: `crates/agent-registry/src/turn_log.rs` (Phase 72).
//! Different table, similar idempotent-on-id semantics.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
pub enum ConfigChangesError {
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("sqlx migrate error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("invalid n parameter: {0}")]
    InvalidN(usize),
}

/// One row of the audit log. `status` is the transition that
/// produced this row; the same `patch_id` may appear multiple
/// times across statuses, but `(patch_id, status)` is unique.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigChangeRow {
    pub patch_id: String,
    pub binding_id: String,
    pub agent_id: String,
    /// `propose` | `apply` | `expire` | `reject`. Identifies the
    /// op that fired this row (one row per transition).
    pub op: String,
    pub key: String,
    /// YAML-rendered value. Caller is responsible for redacting
    /// secrets BEFORE passing to `record` — store does not
    /// inspect strings.
    pub value: Option<String>,
    /// `proposed` | `applied` | `rolled_back` | `rejected` |
    /// `expired`. Pre-rendered so a SQL filter can pick rows of
    /// interest without parsing op transitions.
    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub applied_at: Option<i64>,
}

#[async_trait]
pub trait ConfigChangesStore: Send + Sync + 'static {
    /// Idempotent on `(patch_id, status)` — repeat insert with
    /// the same key is a no-op. Caller passes one row per
    /// state transition.
    async fn record(&self, row: &ConfigChangeRow) -> Result<(), ConfigChangesError>;

    /// Latest `n` rows ordered by `created_at DESC`. Capped at
    /// 200 internally so a runaway tool call cannot pull the full
    /// table into memory.
    async fn tail(&self, n: usize) -> Result<Vec<ConfigChangeRow>, ConfigChangesError>;

    /// Latest row for a given `patch_id`, regardless of status.
    /// Returns `None` when no row exists yet (proposal never
    /// recorded — only happens during a race window between
    /// staging file write and the first `record`).
    async fn get(&self, patch_id: &str) -> Result<Option<ConfigChangeRow>, ConfigChangesError>;
}

const MAX_TAIL_ROWS: usize = 200;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS config_changes (
    patch_id    TEXT NOT NULL,
    status      TEXT NOT NULL,
    binding_id  TEXT NOT NULL,
    agent_id    TEXT NOT NULL,
    op          TEXT NOT NULL,
    key         TEXT NOT NULL,
    value       TEXT,
    error       TEXT,
    created_at  INTEGER NOT NULL,
    applied_at  INTEGER,
    PRIMARY KEY (patch_id, status)
);
CREATE INDEX IF NOT EXISTS idx_config_changes_created_at ON config_changes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_config_changes_patch_id ON config_changes(patch_id);
"#;

pub struct SqliteConfigChangesStore {
    pool: SqlitePool,
}

impl SqliteConfigChangesStore {
    /// Open or create the SQLite store at `url`. Pass
    /// `"sqlite::memory:"` for tests. Schema is idempotent.
    pub async fn open(url: &str) -> Result<Self, ConfigChangesError> {
        let opts = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA_SQL).execute(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_in_memory() -> Result<Self, ConfigChangesError> {
        Self::open("sqlite::memory:").await
    }
}

#[async_trait]
impl ConfigChangesStore for SqliteConfigChangesStore {
    async fn record(&self, row: &ConfigChangeRow) -> Result<(), ConfigChangesError> {
        // ON CONFLICT DO NOTHING — the (patch_id, status) PK
        // makes this idempotent across replays.
        sqlx::query(
            r#"INSERT INTO config_changes
                (patch_id, status, binding_id, agent_id, op, key, value, error, created_at, applied_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(patch_id, status) DO NOTHING"#,
        )
        .bind(&row.patch_id)
        .bind(&row.status)
        .bind(&row.binding_id)
        .bind(&row.agent_id)
        .bind(&row.op)
        .bind(&row.key)
        .bind(&row.value)
        .bind(&row.error)
        .bind(row.created_at)
        .bind(row.applied_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn tail(&self, n: usize) -> Result<Vec<ConfigChangeRow>, ConfigChangesError> {
        let limit = n.clamp(1, MAX_TAIL_ROWS) as i64;
        let rows = sqlx::query_as::<_, ConfigChangeRow>(
            r#"SELECT patch_id, binding_id, agent_id, op, key, value, status, error,
                      created_at, applied_at
                 FROM config_changes
                ORDER BY created_at DESC, rowid DESC
                LIMIT ?"#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn get(&self, patch_id: &str) -> Result<Option<ConfigChangeRow>, ConfigChangesError> {
        let row = sqlx::query_as::<_, ConfigChangeRow>(
            r#"SELECT patch_id, binding_id, agent_id, op, key, value, status, error,
                      created_at, applied_at
                 FROM config_changes
                WHERE patch_id = ?
                ORDER BY created_at DESC, rowid DESC
                LIMIT 1"#,
        )
        .bind(patch_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for ConfigChangeRow {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            patch_id: row.try_get("patch_id")?,
            binding_id: row.try_get("binding_id")?,
            agent_id: row.try_get("agent_id")?,
            op: row.try_get("op")?,
            key: row.try_get("key")?,
            value: row.try_get("value")?,
            status: row.try_get("status")?,
            error: row.try_get("error")?,
            created_at: row.try_get("created_at")?,
            applied_at: row.try_get("applied_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(patch_id: &str, status: &str, created_at: i64) -> ConfigChangeRow {
        ConfigChangeRow {
            patch_id: patch_id.into(),
            binding_id: "wa:default".into(),
            agent_id: "cody".into(),
            op: "propose".into(),
            key: "model.model".into(),
            value: Some("\"claude-opus-4-7\"".into()),
            status: status.into(),
            error: None,
            created_at,
            applied_at: None,
        }
    }

    #[tokio::test]
    async fn open_in_memory_creates_schema() {
        let store = SqliteConfigChangesStore::open_in_memory().await.unwrap();
        let tail = store.tail(10).await.unwrap();
        assert!(tail.is_empty());
    }

    #[tokio::test]
    async fn record_then_tail_returns_rows() {
        let store = SqliteConfigChangesStore::open_in_memory().await.unwrap();
        store.record(&fixture("01J7AAA", "proposed", 100)).await.unwrap();
        store.record(&fixture("01J7BBB", "proposed", 200)).await.unwrap();
        store.record(&fixture("01J7CCC", "proposed", 300)).await.unwrap();
        let tail = store.tail(10).await.unwrap();
        assert_eq!(tail.len(), 3);
        // Newest first.
        assert_eq!(tail[0].patch_id, "01J7CCC");
        assert_eq!(tail[1].patch_id, "01J7BBB");
        assert_eq!(tail[2].patch_id, "01J7AAA");
    }

    #[tokio::test]
    async fn idempotent_on_patch_id_status() {
        let store = SqliteConfigChangesStore::open_in_memory().await.unwrap();
        store.record(&fixture("01J7AAA", "proposed", 100)).await.unwrap();
        // Same (patch_id, status) — no-op.
        store.record(&fixture("01J7AAA", "proposed", 999)).await.unwrap();
        let tail = store.tail(10).await.unwrap();
        assert_eq!(tail.len(), 1);
        // Original timestamp survives.
        assert_eq!(tail[0].created_at, 100);
    }

    #[tokio::test]
    async fn different_status_for_same_patch_id_appends_new_row() {
        let store = SqliteConfigChangesStore::open_in_memory().await.unwrap();
        store.record(&fixture("01J7AAA", "proposed", 100)).await.unwrap();
        store.record(&fixture("01J7AAA", "applied", 200)).await.unwrap();
        let tail = store.tail(10).await.unwrap();
        assert_eq!(tail.len(), 2);
    }

    #[tokio::test]
    async fn get_returns_latest_status_for_patch() {
        let store = SqliteConfigChangesStore::open_in_memory().await.unwrap();
        store.record(&fixture("01J7AAA", "proposed", 100)).await.unwrap();
        store.record(&fixture("01J7AAA", "applied", 200)).await.unwrap();
        let got = store.get("01J7AAA").await.unwrap().unwrap();
        assert_eq!(got.status, "applied");
        assert_eq!(got.created_at, 200);

        let missing = store.get("01J7ZZZ").await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn tail_caps_at_max() {
        let store = SqliteConfigChangesStore::open_in_memory().await.unwrap();
        for i in 0..250 {
            let row = fixture(&format!("p{i:03}"), "proposed", i as i64);
            store.record(&row).await.unwrap();
        }
        // Caller asks for 1000; we cap at 200.
        let tail = store.tail(1000).await.unwrap();
        assert_eq!(tail.len(), MAX_TAIL_ROWS);
    }
}
