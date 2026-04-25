//! SQLite-backed storage for online history compaction.
//!
//! Holds two tables:
//!
//! * `compactions_v1` — append-only audit log. One row per successful
//!   compaction: which session, when it happened, how many turns were
//!   summarized, what the summary said, model used, token cost. Lets
//!   operators rebuild a compacted thread or audit an LLM-generated
//!   summary post-hoc.
//! * `compaction_locks_v1` — single-row-per-session advisory lock so
//!   only one compactor at a time can run on a given session
//!   (multi-process safe). Locks have a TTL: if a process crashes
//!   mid-compaction, the next acquire after `ttl_seconds` automatically
//!   evicts the stale entry.
//!
//! Table names carry a `_v1` suffix to keep room for a schema bump
//! without colliding with stale rows in long-running deployments.

use anyhow::Result;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;
use std::str::FromStr;
use uuid::Uuid;

/// One persisted compaction event. `summary` is the LLM-generated
/// replacement text; `tail_start_index` is the first session-history
/// index that was preserved verbatim (everything before was folded
/// into the summary).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CompactionRow {
    pub session_id: String,
    pub compacted_at: i64, // unix ms
    pub head_turn_count: i64,
    pub tail_start_index: i64,
    pub summary: String,
    pub model_used: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

pub struct CompactionStore {
    pool: SqlitePool,
}

impl CompactionStore {
    /// Open or create the SQLite file at `db_path`. The directory must
    /// already exist (we don't create it). Use `:memory:` for tests.
    pub async fn open(db_path: &str) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(db_path)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePool::connect_with(opts).await?;
        sqlx::query("PRAGMA foreign_keys=ON").execute(&pool).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// Wrap an externally-opened pool. Useful when sharing the same
    /// SQLite file as another store. Caller is responsible for migrate.
    pub fn with_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS compactions_v1 (
                session_id        TEXT NOT NULL,
                compacted_at      INTEGER NOT NULL,
                head_turn_count   INTEGER NOT NULL,
                tail_start_index  INTEGER NOT NULL,
                summary           TEXT NOT NULL,
                model_used        TEXT NOT NULL,
                input_tokens      INTEGER NOT NULL,
                output_tokens     INTEGER NOT NULL,
                PRIMARY KEY (session_id, compacted_at)
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_compactions_v1_session
             ON compactions_v1(session_id, compacted_at DESC)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS compaction_locks_v1 (
                session_id TEXT PRIMARY KEY,
                locked_at  INTEGER NOT NULL,
                holder     TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Append a successful compaction. Errors only on storage problems.
    pub async fn insert(&self, row: &CompactionRow) -> Result<()> {
        sqlx::query(
            "INSERT INTO compactions_v1 (
                session_id, compacted_at, head_turn_count, tail_start_index,
                summary, model_used, input_tokens, output_tokens
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.session_id)
        .bind(row.compacted_at)
        .bind(row.head_turn_count)
        .bind(row.tail_start_index)
        .bind(&row.summary)
        .bind(&row.model_used)
        .bind(row.input_tokens)
        .bind(row.output_tokens)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Most recent compaction for a session, if any.
    pub async fn latest(&self, session_id: Uuid) -> Result<Option<CompactionRow>> {
        let row: Option<CompactionRow> = sqlx::query_as(
            "SELECT session_id, compacted_at, head_turn_count, tail_start_index,
                    summary, model_used, input_tokens, output_tokens
             FROM compactions_v1
             WHERE session_id = ?
             ORDER BY compacted_at DESC
             LIMIT 1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// All compactions for a session, newest first, capped at `limit`.
    pub async fn list_for_session(
        &self,
        session_id: Uuid,
        limit: u32,
    ) -> Result<Vec<CompactionRow>> {
        let rows: Vec<CompactionRow> = sqlx::query_as(
            "SELECT session_id, compacted_at, head_turn_count, tail_start_index,
                    summary, model_used, input_tokens, output_tokens
             FROM compactions_v1
             WHERE session_id = ?
             ORDER BY compacted_at DESC
             LIMIT ?",
        )
        .bind(session_id.to_string())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Try to take the per-session compaction lock. Returns `true`
    /// when acquired, `false` when another holder is active. Stale
    /// locks (older than `ttl_seconds`) are evicted before the
    /// acquire attempt so a crashed compactor doesn't deadlock the
    /// session forever.
    ///
    /// `holder` is a free-form debug label (`"pid:thread_id"` works
    /// well) recorded on the row to make orphan diagnosis easier.
    pub async fn try_acquire_lock(
        &self,
        session_id: Uuid,
        holder: &str,
        ttl_seconds: u32,
    ) -> Result<bool> {
        // Sweep stale locks for this session before attempting the
        // INSERT — keeps the per-acquire footprint cheap and avoids a
        // global cleanup pass.
        let now_ms = chrono::Utc::now().timestamp_millis();
        let cutoff_ms = now_ms - (ttl_seconds as i64 * 1000);
        sqlx::query(
            "DELETE FROM compaction_locks_v1
             WHERE session_id = ? AND locked_at < ?",
        )
        .bind(session_id.to_string())
        .bind(cutoff_ms)
        .execute(&self.pool)
        .await?;
        // INSERT … OR IGNORE: when another holder owns the lock the
        // PRIMARY KEY constraint silently rejects the insert and we
        // report failure to the caller.
        let result = sqlx::query(
            "INSERT OR IGNORE INTO compaction_locks_v1 (session_id, locked_at, holder)
             VALUES (?, ?, ?)",
        )
        .bind(session_id.to_string())
        .bind(now_ms)
        .bind(holder)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Release a previously-held lock. Idempotent.
    pub async fn release_lock(&self, session_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM compaction_locks_v1 WHERE session_id = ?")
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Sweep every lock older than `ttl_seconds`. Call from a
    /// background task or on boot to clean up after crashed
    /// processes. Returns the number of rows removed.
    pub async fn cleanup_stale_locks(&self, ttl_seconds: u32) -> Result<u64> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let cutoff_ms = now_ms - (ttl_seconds as i64 * 1000);
        let result = sqlx::query("DELETE FROM compaction_locks_v1 WHERE locked_at < ?")
            .bind(cutoff_ms)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open_mem() -> CompactionStore {
        CompactionStore::open(":memory:").await.unwrap()
    }

    fn row(session: Uuid, ts: i64) -> CompactionRow {
        CompactionRow {
            session_id: session.to_string(),
            compacted_at: ts,
            head_turn_count: 5,
            tail_start_index: 5,
            summary: "Compacted: discussed weather.".into(),
            model_used: "claude-sonnet-4-5".into(),
            input_tokens: 10_000,
            output_tokens: 800,
        }
    }

    #[tokio::test]
    async fn migrate_is_idempotent() {
        let s = open_mem().await;
        s.migrate().await.unwrap();
        s.migrate().await.unwrap();
    }

    #[tokio::test]
    async fn insert_and_latest_roundtrip() {
        let s = open_mem().await;
        let session = Uuid::new_v4();
        s.insert(&row(session, 1)).await.unwrap();
        s.insert(&row(session, 2)).await.unwrap();
        let latest = s.latest(session).await.unwrap().unwrap();
        assert_eq!(latest.compacted_at, 2);
    }

    #[tokio::test]
    async fn list_for_session_orders_newest_first() {
        let s = open_mem().await;
        let session = Uuid::new_v4();
        for i in [3, 1, 2] {
            s.insert(&row(session, i)).await.unwrap();
        }
        let rows = s.list_for_session(session, 10).await.unwrap();
        let timestamps: Vec<i64> = rows.iter().map(|r| r.compacted_at).collect();
        assert_eq!(timestamps, vec![3, 2, 1]);
    }

    #[tokio::test]
    async fn list_respects_limit() {
        let s = open_mem().await;
        let session = Uuid::new_v4();
        for i in 0..5 {
            s.insert(&row(session, i)).await.unwrap();
        }
        let rows = s.list_for_session(session, 2).await.unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn lock_acquire_and_double_acquire_blocks() {
        let s = open_mem().await;
        let session = Uuid::new_v4();
        assert!(s.try_acquire_lock(session, "p1", 60).await.unwrap());
        assert!(!s.try_acquire_lock(session, "p2", 60).await.unwrap());
    }

    #[tokio::test]
    async fn release_unlocks() {
        let s = open_mem().await;
        let session = Uuid::new_v4();
        assert!(s.try_acquire_lock(session, "p1", 60).await.unwrap());
        s.release_lock(session).await.unwrap();
        assert!(s.try_acquire_lock(session, "p1", 60).await.unwrap());
    }

    #[tokio::test]
    async fn release_is_idempotent() {
        let s = open_mem().await;
        let session = Uuid::new_v4();
        s.release_lock(session).await.unwrap();
        s.release_lock(session).await.unwrap();
    }

    #[tokio::test]
    async fn stale_lock_evicted_on_acquire() {
        let s = open_mem().await;
        let session = Uuid::new_v4();
        // Plant an artificially old lock by direct insert at ts=0.
        sqlx::query(
            "INSERT INTO compaction_locks_v1 (session_id, locked_at, holder)
             VALUES (?, 0, 'crashed-process')",
        )
        .bind(session.to_string())
        .execute(&s.pool)
        .await
        .unwrap();
        // ttl=1 second → way past the cutoff → next acquire wins.
        assert!(s.try_acquire_lock(session, "p2", 1).await.unwrap());
    }

    #[tokio::test]
    async fn cleanup_stale_locks_returns_count() {
        let s = open_mem().await;
        sqlx::query(
            "INSERT INTO compaction_locks_v1 (session_id, locked_at, holder)
             VALUES (?, 0, 'crashed1')",
        )
        .bind(Uuid::new_v4().to_string())
        .execute(&s.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO compaction_locks_v1 (session_id, locked_at, holder)
             VALUES (?, 0, 'crashed2')",
        )
        .bind(Uuid::new_v4().to_string())
        .execute(&s.pool)
        .await
        .unwrap();
        // Plant one fresh lock that should NOT be swept.
        let fresh = Uuid::new_v4();
        s.try_acquire_lock(fresh, "live", 60).await.unwrap();
        let removed = s.cleanup_stale_locks(60).await.unwrap();
        assert_eq!(removed, 2);
        // Fresh lock survives.
        assert!(!s.try_acquire_lock(fresh, "other", 60).await.unwrap());
    }
}
