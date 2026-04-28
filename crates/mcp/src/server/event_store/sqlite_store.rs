//! Phase 76.8 — SQLite-backed `SessionEventStore`.
//!
//! Mirrors `crates/mcp/src/server/audit_log/sqlite_store.rs` (Phase
//! 76.11) — WAL + synchronous=NORMAL, idempotent INSERT OR IGNORE
//! on the `(session_id, seq)` composite key.

use async_trait::async_trait;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use super::store::{EventStoreError, SessionEventStore};
use super::types::StoredEvent;

pub struct SqliteSessionEventStore {
    pool: SqlitePool,
}

impl SqliteSessionEventStore {
    pub async fn open(path: &str) -> Result<Self, EventStoreError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let max_conns = if path == ":memory:" { 1 } else { 4 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(opts)
            .await
            .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        if path != ":memory:" {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&pool)
                .await
                .map_err(|e| EventStoreError::Backend(e.to_string()))?;
            sqlx::query("PRAGMA synchronous = NORMAL")
                .execute(&pool)
                .await
                .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        }
        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_memory() -> Result<Self, EventStoreError> {
        Self::open(":memory:").await
    }

    async fn migrate(pool: &SqlitePool) -> Result<(), EventStoreError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS mcp_session_events (\
                session_id    TEXT NOT NULL,\
                seq           INTEGER NOT NULL,\
                frame_json    TEXT NOT NULL,\
                created_at_ms INTEGER NOT NULL,\
                PRIMARY KEY (session_id, seq)\
            ) WITHOUT ROWID",
        )
        .execute(pool)
        .await
        .map_err(|e| EventStoreError::Backend(e.to_string()))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_mcp_events_created \
                ON mcp_session_events(created_at_ms)",
        )
        .execute(pool)
        .await
        .map_err(|e| EventStoreError::Backend(e.to_string()))?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS mcp_session_subscriptions (\
                session_id TEXT NOT NULL,\
                uri        TEXT NOT NULL,\
                PRIMARY KEY (session_id, uri)\
            ) WITHOUT ROWID",
        )
        .execute(pool)
        .await
        .map_err(|e| EventStoreError::Backend(e.to_string()))?;

        Ok(())
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[async_trait]
impl SessionEventStore for SqliteSessionEventStore {
    async fn append(
        &self,
        session_id: &str,
        seq: u64,
        frame: &Value,
    ) -> Result<(), EventStoreError> {
        let payload =
            serde_json::to_string(frame).map_err(|e| EventStoreError::Invalid(e.to_string()))?;
        let seq_i = seq as i64;
        let created = now_ms();
        sqlx::query(
            "INSERT OR IGNORE INTO mcp_session_events \
             (session_id, seq, frame_json, created_at_ms) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(seq_i)
        .bind(payload)
        .bind(created)
        .execute(&self.pool)
        .await
        .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn tail_after(
        &self,
        session_id: &str,
        min_seq: u64,
        max_rows: usize,
    ) -> Result<Vec<StoredEvent>, EventStoreError> {
        let min_i = min_seq as i64;
        let limit = max_rows as i64;
        let rows = sqlx::query(
            "SELECT seq, frame_json, created_at_ms \
             FROM mcp_session_events \
             WHERE session_id = ? AND seq > ? \
             ORDER BY seq ASC LIMIT ?",
        )
        .bind(session_id)
        .bind(min_i)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EventStoreError::Backend(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let seq: i64 = r
                .try_get("seq")
                .map_err(|e| EventStoreError::Backend(e.to_string()))?;
            let body: String = r
                .try_get("frame_json")
                .map_err(|e| EventStoreError::Backend(e.to_string()))?;
            let created: i64 = r
                .try_get("created_at_ms")
                .map_err(|e| EventStoreError::Backend(e.to_string()))?;
            let frame: Value =
                serde_json::from_str(&body).map_err(|e| EventStoreError::Invalid(e.to_string()))?;
            out.push(StoredEvent {
                seq: seq as u64,
                frame,
                created_at_ms: created,
            });
        }
        Ok(out)
    }

    async fn drop_session(&self, session_id: &str) -> Result<u64, EventStoreError> {
        let res = sqlx::query("DELETE FROM mcp_session_events WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        let removed = res.rows_affected();
        sqlx::query("DELETE FROM mcp_session_subscriptions WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        Ok(removed)
    }

    async fn purge_older_than(&self, before_ms: i64) -> Result<u64, EventStoreError> {
        let res = sqlx::query("DELETE FROM mcp_session_events WHERE created_at_ms < ?")
            .bind(before_ms)
            .execute(&self.pool)
            .await
            .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        Ok(res.rows_affected())
    }

    async fn purge_oldest_for_session(
        &self,
        session_id: &str,
        keep: u64,
    ) -> Result<u64, EventStoreError> {
        let keep_i = keep as i64;
        // SQLite + sqlx: subselect via correlated query keeps the
        // newest `keep` rows by descending seq, deletes the rest.
        let res = sqlx::query(
            "DELETE FROM mcp_session_events \
             WHERE session_id = ? AND seq IN (\
                 SELECT seq FROM mcp_session_events \
                 WHERE session_id = ? \
                 ORDER BY seq ASC \
                 LIMIT MAX(0, (SELECT COUNT(*) FROM mcp_session_events \
                               WHERE session_id = ?) - ?)\
             )",
        )
        .bind(session_id)
        .bind(session_id)
        .bind(session_id)
        .bind(keep_i)
        .execute(&self.pool)
        .await
        .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        Ok(res.rows_affected())
    }

    async fn put_subscriptions(
        &self,
        session_id: &str,
        uris: &[String],
    ) -> Result<(), EventStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        sqlx::query("DELETE FROM mcp_session_subscriptions WHERE session_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        for uri in uris {
            sqlx::query(
                "INSERT OR IGNORE INTO mcp_session_subscriptions (session_id, uri) \
                 VALUES (?, ?)",
            )
            .bind(session_id)
            .bind(uri)
            .execute(&mut *tx)
            .await
            .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn load_subscriptions(&self, session_id: &str) -> Result<Vec<String>, EventStoreError> {
        let rows = sqlx::query(
            "SELECT uri FROM mcp_session_subscriptions WHERE session_id = ? ORDER BY uri ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EventStoreError::Backend(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let u: String = r
                .try_get("uri")
                .map_err(|e| EventStoreError::Backend(e.to_string()))?;
            out.push(u);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn store() -> SqliteSessionEventStore {
        SqliteSessionEventStore::open_memory().await.unwrap()
    }

    #[tokio::test]
    async fn append_then_tail_after_returns_strict_gap() {
        let s = store().await;
        for seq in 1..=5u64 {
            s.append("sid", seq, &json!({"n": seq})).await.unwrap();
        }
        let out = s.tail_after("sid", 2, 100).await.unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].seq, 3);
        assert_eq!(out[2].seq, 5);
    }

    #[tokio::test]
    async fn append_idempotent_on_seq_collision() {
        let s = store().await;
        s.append("sid", 1, &json!({"v": "first"})).await.unwrap();
        s.append("sid", 1, &json!({"v": "second"})).await.unwrap();
        let out = s.tail_after("sid", 0, 100).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].frame, json!({"v": "first"}));
    }

    #[tokio::test]
    async fn tail_after_empty_when_no_session() {
        let s = store().await;
        let out = s.tail_after("missing", 0, 100).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn tail_after_respects_max_rows() {
        let s = store().await;
        for seq in 1..=10u64 {
            s.append("sid", seq, &json!({"n": seq})).await.unwrap();
        }
        let out = s.tail_after("sid", 0, 3).await.unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out.last().unwrap().seq, 3);
    }

    #[tokio::test]
    async fn drop_session_removes_events_and_subs() {
        let s = store().await;
        s.append("sid", 1, &json!({})).await.unwrap();
        s.append("sid", 2, &json!({})).await.unwrap();
        s.put_subscriptions("sid", &["res://a".into()])
            .await
            .unwrap();
        let removed = s.drop_session("sid").await.unwrap();
        assert_eq!(removed, 2);
        assert!(s.tail_after("sid", 0, 10).await.unwrap().is_empty());
        assert!(s.load_subscriptions("sid").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn purge_older_than_drops_only_stale() {
        let s = store().await;
        s.append("sid", 1, &json!({})).await.unwrap();
        // bump clock — directly poke via raw SQL since now_ms() runs
        // in the test process. We just assert that "future" cutoff
        // wipes everything.
        let removed = s.purge_older_than(now_ms() + 60_000).await.unwrap();
        assert_eq!(removed, 1);
    }

    #[tokio::test]
    async fn purge_oldest_for_session_keeps_n() {
        let s = store().await;
        for seq in 1..=10u64 {
            s.append("sid", seq, &json!({})).await.unwrap();
        }
        let removed = s.purge_oldest_for_session("sid", 4).await.unwrap();
        assert_eq!(removed, 6);
        let kept = s.tail_after("sid", 0, 100).await.unwrap();
        assert_eq!(kept.len(), 4);
        assert_eq!(kept[0].seq, 7);
    }

    #[tokio::test]
    async fn subscriptions_replace_semantics() {
        let s = store().await;
        s.put_subscriptions("sid", &["a".into(), "b".into()])
            .await
            .unwrap();
        s.put_subscriptions("sid", &["b".into(), "c".into()])
            .await
            .unwrap();
        let mut got = s.load_subscriptions("sid").await.unwrap();
        got.sort();
        assert_eq!(got, vec!["b", "c"]);
    }
}
