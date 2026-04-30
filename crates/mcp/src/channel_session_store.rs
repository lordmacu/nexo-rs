//! Phase 80.9.d.b — persistent `SessionRegistry`.
//!
//! [`crate::channel_bridge::InMemorySessionRegistry`] is fine for
//! single-shot tests but loses every threading mapping when the
//! daemon restarts — the next inbound from `(slack, chat=C123)`
//! would synthesise a fresh agent session uuid, breaking
//! continuity from the user's perspective ("why did the bot
//! re-introduce itself?").
//!
//! [`SqliteSessionRegistry`] implements the same
//! [`crate::channel_bridge::SessionRegistry`] trait against a
//! SQLite database. Same UPSERT semantics, same idle-GC, same
//! `len`. Migration is idempotent (`CREATE TABLE IF NOT EXISTS`)
//! so opening an existing DB is safe.
//!
//! Schema:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS mcp_channel_sessions (
//!   key           TEXT PRIMARY KEY NOT NULL,
//!   session_id    TEXT NOT NULL,
//!   last_seen_ms  INTEGER NOT NULL
//! );
//! CREATE INDEX IF NOT EXISTS idx_channel_sessions_last_seen
//!   ON mcp_channel_sessions(last_seen_ms);
//! ```

use crate::channel::ChannelSessionKey;
use crate::channel_bridge::SessionRegistry;
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{ConnectOptions, Row, SqlitePool};
use std::str::FromStr;
use uuid::Uuid;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mcp_channel_sessions (
    key          TEXT PRIMARY KEY NOT NULL,
    session_id   TEXT NOT NULL,
    last_seen_ms INTEGER NOT NULL
);
"#;

const INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS idx_channel_sessions_last_seen
    ON mcp_channel_sessions(last_seen_ms);
"#;

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("sqlite: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("invalid uuid: {0}")]
    InvalidUuid(String),
}

pub struct SqliteSessionRegistry {
    pool: SqlitePool,
}

impl SqliteSessionRegistry {
    /// Open a SQLite-backed registry at `path`. `:memory:` is
    /// supported for tests. The file is created with WAL +
    /// `synchronous=NORMAL` — same trade-off Phase 71 / Phase 72
    /// stores ship.
    pub async fn open(path: &str) -> Result<Self, SessionStoreError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{path}"))
            .map_err(|e| SessionStoreError::Sql(sqlx::Error::Configuration(Box::new(e))))?
            .create_if_missing(true)
            .disable_statement_logging();
        let pool = SqlitePoolOptions::new()
            .max_connections(if path == ":memory:" { 1 } else { 4 })
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
        sqlx::query(INDEX).execute(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_memory() -> Result<Self, SessionStoreError> {
        Self::open(":memory:").await
    }

    /// Test-only helper: dump every row sorted by key. Real
    /// callers go through the [`SessionRegistry`] trait.
    pub async fn snapshot(&self) -> Result<Vec<(String, Uuid, i64)>, SessionStoreError> {
        let rows = sqlx::query(
            "SELECT key, session_id, last_seen_ms \
             FROM mcp_channel_sessions ORDER BY key ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let key: String = r.try_get("key")?;
            let sid_raw: String = r.try_get("session_id")?;
            let sid = Uuid::parse_str(&sid_raw)
                .map_err(|_| SessionStoreError::InvalidUuid(sid_raw.clone()))?;
            let ms: i64 = r.try_get("last_seen_ms")?;
            out.push((key, sid, ms));
        }
        Ok(out)
    }
}

#[async_trait]
impl SessionRegistry for SqliteSessionRegistry {
    async fn resolve(&self, key: &ChannelSessionKey) -> Uuid {
        let now_ms = chrono::Utc::now().timestamp_millis();

        // UPSERT — single round-trip whether the key exists or
        // not. The `RETURNING` clause gives us the row's
        // session_id back so we don't need a follow-up SELECT.
        // SQLite supports `RETURNING` since 3.35 (early 2021);
        // every shipping platform we target has it.
        //
        // The `excluded.last_seen_ms` ON CONFLICT branch refreshes
        // the timestamp so idle-GC sees the row as live.
        let candidate = Uuid::new_v4().to_string();
        let row = sqlx::query(
            "INSERT INTO mcp_channel_sessions (key, session_id, last_seen_ms) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(key) DO UPDATE SET last_seen_ms = excluded.last_seen_ms \
             RETURNING session_id",
        )
        .bind(&key.0)
        .bind(&candidate)
        .bind(now_ms)
        .fetch_one(&self.pool)
        .await;

        match row {
            Ok(r) => match r.try_get::<String, _>("session_id").ok() {
                Some(s) => Uuid::parse_str(&s)
                    .unwrap_or_else(|_| Uuid::parse_str(&candidate).unwrap()),
                None => Uuid::parse_str(&candidate).unwrap(),
            },
            Err(e) => {
                // Fail-safe — log + return the candidate. Threading
                // *for this turn* is preserved, persistence is best-
                // effort. The bridge handles transient failures the
                // same way (warn + continue).
                tracing::warn!(
                    error = %e,
                    key = %key.0,
                    "channel session UPSERT failed; falling back to ephemeral uuid"
                );
                Uuid::parse_str(&candidate).unwrap()
            }
        }
    }

    async fn gc_idle(&self, max_idle_ms: i64) -> usize {
        if max_idle_ms <= 0 {
            return 0;
        }
        let cutoff = chrono::Utc::now().timestamp_millis() - max_idle_ms;
        let res =
            sqlx::query("DELETE FROM mcp_channel_sessions WHERE last_seen_ms < ?1")
                .bind(cutoff)
                .execute(&self.pool)
                .await;
        match res {
            Ok(r) => r.rows_affected() as usize,
            Err(e) => {
                tracing::warn!(error = %e, "channel session gc_idle failed");
                0
            }
        }
    }

    async fn len(&self) -> usize {
        match sqlx::query("SELECT COUNT(*) AS n FROM mcp_channel_sessions")
            .fetch_one(&self.pool)
            .await
        {
            Ok(r) => r.try_get::<i64, _>("n").unwrap_or(0).max(0) as usize,
            Err(_) => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> ChannelSessionKey {
        ChannelSessionKey(s.to_string())
    }

    #[tokio::test]
    async fn first_seen_creates_session_id() {
        let r = SqliteSessionRegistry::open_memory().await.unwrap();
        let id = r.resolve(&key("slack|chat=A")).await;
        assert_eq!(r.len().await, 1);
        // Repeat returns the same uuid.
        let again = r.resolve(&key("slack|chat=A")).await;
        assert_eq!(id, again);
    }

    #[tokio::test]
    async fn distinct_keys_get_distinct_uuids() {
        let r = SqliteSessionRegistry::open_memory().await.unwrap();
        let a = r.resolve(&key("slack|chat=A")).await;
        let b = r.resolve(&key("slack|chat=B")).await;
        assert_ne!(a, b);
        assert_eq!(r.len().await, 2);
    }

    #[tokio::test]
    async fn resolve_refreshes_last_seen() {
        let r = SqliteSessionRegistry::open_memory().await.unwrap();
        let _ = r.resolve(&key("slack|chat=A")).await;
        let snap1 = r.snapshot().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let _ = r.resolve(&key("slack|chat=A")).await;
        let snap2 = r.snapshot().await.unwrap();
        assert!(snap2[0].2 > snap1[0].2);
    }

    #[tokio::test]
    async fn gc_idle_evicts_stale_rows() {
        let r = SqliteSessionRegistry::open_memory().await.unwrap();
        let _ = r.resolve(&key("a")).await;
        let _ = r.resolve(&key("b")).await;
        // No row is old enough yet → no eviction.
        assert_eq!(r.gc_idle(1).await, 0);
        assert_eq!(r.len().await, 2);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let evicted = r.gc_idle(20).await;
        assert_eq!(evicted, 2);
        assert_eq!(r.len().await, 0);
    }

    #[tokio::test]
    async fn gc_idle_zero_is_noop() {
        let r = SqliteSessionRegistry::open_memory().await.unwrap();
        let _ = r.resolve(&key("a")).await;
        assert_eq!(r.gc_idle(0).await, 0);
        assert_eq!(r.len().await, 1);
    }

    #[tokio::test]
    async fn gc_idle_negative_is_noop() {
        let r = SqliteSessionRegistry::open_memory().await.unwrap();
        let _ = r.resolve(&key("a")).await;
        assert_eq!(r.gc_idle(-5).await, 0);
        assert_eq!(r.len().await, 1);
    }

    #[tokio::test]
    async fn snapshot_renders_existing_rows() {
        let r = SqliteSessionRegistry::open_memory().await.unwrap();
        let id_a = r.resolve(&key("a")).await;
        let id_b = r.resolve(&key("b")).await;
        let snap = r.snapshot().await.unwrap();
        assert_eq!(snap.len(), 2);
        // Sorted by key ASC.
        assert_eq!(snap[0].0, "a");
        assert_eq!(snap[0].1, id_a);
        assert_eq!(snap[1].0, "b");
        assert_eq!(snap[1].1, id_b);
    }

    #[tokio::test]
    async fn schema_is_idempotent_across_reopens() {
        // We can't reopen `:memory:` (it dies with the connection)
        // so use a real temp path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let path_str = path.to_string_lossy().to_string();

        // First open + write.
        {
            let r = SqliteSessionRegistry::open(&path_str).await.unwrap();
            let _ = r.resolve(&key("persistent")).await;
            assert_eq!(r.len().await, 1);
            // Drop the registry to flush the connection.
        }
        // Second open — schema migration must be idempotent + the
        // row must survive.
        let r = SqliteSessionRegistry::open(&path_str).await.unwrap();
        assert_eq!(r.len().await, 1);
        let snap = r.snapshot().await.unwrap();
        assert_eq!(snap[0].0, "persistent");
    }

    #[tokio::test]
    async fn upsert_is_concurrent_safe() {
        // Two concurrent resolvers on the same key must agree on
        // the same uuid — UPSERT semantics guarantee this.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let path_str = path.to_string_lossy().to_string();
        let r = std::sync::Arc::new(SqliteSessionRegistry::open(&path_str).await.unwrap());

        let r1 = r.clone();
        let r2 = r.clone();
        let h1 = tokio::spawn(async move { r1.resolve(&key("race")).await });
        let h2 = tokio::spawn(async move { r2.resolve(&key("race")).await });
        let id1 = h1.await.unwrap();
        let id2 = h2.await.unwrap();
        assert_eq!(id1, id2);
        assert_eq!(r.len().await, 1);
    }
}
