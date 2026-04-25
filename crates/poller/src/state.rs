//! SQLite-backed cursor + lease store.
//!
//! Schema lives in `migrations/001_init.sql`. The pool is a wrapper
//! around `sqlx::SqlitePool` so the runner can share it with anything
//! else that points at the same DB (TaskFlow shares this pattern).
//!
//! Concurrency model:
//! - Writes serialise through the pool (single writer in WAL mode).
//! - Lease acquisition is an UPDATE … WHERE that returns the affected
//!   row count. Cross-process safe: two daemons against the same file
//!   will see one of the UPDATEs win.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};

const MIGRATION: &str = include_str!("../migrations/001_init.sql");

/// Strip `--` line comments, split on `;`, run each non-empty
/// statement individually. SQLite rejects multi-statement queries
/// through `sqlx::query`, so we hand-feed them one at a time.
async fn run_migration(pool: &SqlitePool) -> Result<()> {
    let cleaned: String = MIGRATION
        .lines()
        .map(|l| {
            // Comments may end mid-line — split on `--` and keep what's before.
            l.split("--").next().unwrap_or("").trim_end()
        })
        .collect::<Vec<_>>()
        .join("\n");
    for stmt in cleaned.split(';') {
        let s = stmt.trim();
        if s.is_empty() {
            continue;
        }
        sqlx::query(s)
            .execute(pool)
            .await
            .with_context(|| format!("migration statement failed: {s}"))?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct PollState {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Default)]
pub struct JobStateSnapshot {
    pub job_id: String,
    pub cursor: Option<Vec<u8>>,
    pub last_run_at_ms: Option<i64>,
    pub next_run_at_ms: Option<i64>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub last_duration_ms: Option<i64>,
    pub consecutive_errors: i64,
    pub items_seen_total: i64,
    pub items_dispatched_total: i64,
    pub paused: bool,
    pub last_failure_alert_at_ms: Option<i64>,
}

impl PollState {
    /// Open (or create) the SQLite file at `path`. Runs the bundled
    /// migration on every open — `CREATE TABLE IF NOT EXISTS` is
    /// idempotent.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await
            .with_context(|| format!("open sqlite at {}", path.display()))?;

        run_migration(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory store for tests. Does not persist across opens.
    pub async fn open_in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(":memory:")
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1) // memory DB cannot share connections
            .connect_with(opts)
            .await?;
        run_migration(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn load(&self, job_id: &str) -> Result<Option<JobStateSnapshot>> {
        let row = sqlx::query(
            "SELECT cursor, last_run_at, next_run_at, last_status, last_error,
                    last_duration_ms, consecutive_errors, items_seen_total,
                    items_dispatched_total, paused, last_failure_alert_at
             FROM poll_state WHERE job_id = ?",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        Ok(Some(JobStateSnapshot {
            job_id: job_id.to_string(),
            cursor: r.try_get::<Option<Vec<u8>>, _>("cursor")?,
            last_run_at_ms: r.try_get("last_run_at")?,
            next_run_at_ms: r.try_get("next_run_at")?,
            last_status: r.try_get("last_status")?,
            last_error: r.try_get("last_error")?,
            last_duration_ms: r.try_get("last_duration_ms")?,
            consecutive_errors: r.try_get("consecutive_errors")?,
            items_seen_total: r.try_get("items_seen_total")?,
            items_dispatched_total: r.try_get("items_dispatched_total")?,
            paused: r.try_get::<i64, _>("paused")? != 0,
            last_failure_alert_at_ms: r.try_get("last_failure_alert_at")?,
        }))
    }

    pub async fn list(&self) -> Result<Vec<JobStateSnapshot>> {
        let rows = sqlx::query(
            "SELECT job_id, cursor, last_run_at, next_run_at, last_status, last_error,
                    last_duration_ms, consecutive_errors, items_seen_total,
                    items_dispatched_total, paused, last_failure_alert_at
             FROM poll_state ORDER BY job_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(JobStateSnapshot {
                job_id: r.try_get("job_id")?,
                cursor: r.try_get::<Option<Vec<u8>>, _>("cursor")?,
                last_run_at_ms: r.try_get("last_run_at")?,
                next_run_at_ms: r.try_get("next_run_at")?,
                last_status: r.try_get("last_status")?,
                last_error: r.try_get("last_error")?,
                last_duration_ms: r.try_get("last_duration_ms")?,
                consecutive_errors: r.try_get("consecutive_errors")?,
                items_seen_total: r.try_get("items_seen_total")?,
                items_dispatched_total: r.try_get("items_dispatched_total")?,
                paused: r.try_get::<i64, _>("paused")? != 0,
                last_failure_alert_at_ms: r.try_get("last_failure_alert_at")?,
            });
        }
        Ok(out)
    }

    pub async fn save_tick_ok(
        &self,
        job_id: &str,
        cursor: Option<&[u8]>,
        items_seen: u32,
        items_dispatched: u32,
        duration_ms: i64,
        next_run_at_ms: i64,
        now_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO poll_state(job_id, cursor, last_run_at, next_run_at,
                last_status, last_error, last_duration_ms,
                consecutive_errors, items_seen_total, items_dispatched_total,
                paused, updated_at)
             VALUES(?1, ?2, ?3, ?4, 'ok', NULL, ?5, 0, ?6, ?7, 0, ?3)
             ON CONFLICT(job_id) DO UPDATE SET
                cursor = COALESCE(?2, poll_state.cursor),
                last_run_at = ?3,
                next_run_at = ?4,
                last_status = 'ok',
                last_error = NULL,
                last_duration_ms = ?5,
                consecutive_errors = 0,
                items_seen_total = poll_state.items_seen_total + ?6,
                items_dispatched_total = poll_state.items_dispatched_total + ?7,
                updated_at = ?3",
        )
        .bind(job_id)
        .bind(cursor)
        .bind(now_ms)
        .bind(next_run_at_ms)
        .bind(duration_ms)
        .bind(items_seen as i64)
        .bind(items_dispatched as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn save_tick_err(
        &self,
        job_id: &str,
        status: &str, // "transient" | "permanent" | "skipped"
        error: &str,
        next_run_at_ms: i64,
        duration_ms: i64,
        now_ms: i64,
        bump_consecutive: bool,
    ) -> Result<()> {
        let bump = if bump_consecutive { 1 } else { 0 };
        sqlx::query(
            "INSERT INTO poll_state(job_id, last_run_at, next_run_at, last_status,
                last_error, last_duration_ms, consecutive_errors, paused, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?2)
             ON CONFLICT(job_id) DO UPDATE SET
                last_run_at = ?2,
                next_run_at = ?3,
                last_status = ?4,
                last_error = ?5,
                last_duration_ms = ?6,
                consecutive_errors = poll_state.consecutive_errors + ?7,
                updated_at = ?2",
        )
        .bind(job_id)
        .bind(now_ms)
        .bind(next_run_at_ms)
        .bind(status)
        .bind(error)
        .bind(duration_ms)
        .bind(bump)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_paused(&self, job_id: &str, paused: bool, now_ms: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO poll_state(job_id, paused, updated_at)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(job_id) DO UPDATE SET paused = ?2, updated_at = ?3",
        )
        .bind(job_id)
        .bind(if paused { 1 } else { 0 })
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn reset_cursor(&self, job_id: &str, now_ms: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO poll_state(job_id, cursor, consecutive_errors, paused, updated_at)
             VALUES(?1, NULL, 0, 0, ?2)
             ON CONFLICT(job_id) DO UPDATE SET
                cursor = NULL,
                consecutive_errors = 0,
                last_error = NULL,
                paused = 0,
                updated_at = ?2",
        )
        .bind(job_id)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_failure_alert(&self, job_id: &str, now_ms: i64) -> Result<()> {
        sqlx::query(
            "UPDATE poll_state SET last_failure_alert_at = ?1, updated_at = ?1 WHERE job_id = ?2",
        )
        .bind(now_ms)
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Atomic lease acquisition. Returns true when this caller now
    /// owns the lease until `running_until_ms`. Cross-process safe.
    pub async fn acquire_lease(
        &self,
        job_id: &str,
        leaseholder: &str,
        running_until_ms: i64,
        now_ms: i64,
    ) -> Result<bool> {
        // Try to insert; if there's a conflict, try to take over an
        // expired lease. Both paths return rowcount=1 on success.
        let inserted = sqlx::query(
            "INSERT INTO poll_lease(job_id, leaseholder, running_until)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(job_id) DO UPDATE SET
                 leaseholder = ?2,
                 running_until = ?3
             WHERE poll_lease.running_until <= ?4",
        )
        .bind(job_id)
        .bind(leaseholder)
        .bind(running_until_ms)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(inserted.rows_affected() > 0)
    }

    pub async fn release_lease(&self, job_id: &str, leaseholder: &str) -> Result<()> {
        sqlx::query("DELETE FROM poll_lease WHERE job_id = ?1 AND leaseholder = ?2")
            .bind(job_id)
            .bind(leaseholder)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn lease_holder(&self, job_id: &str) -> Result<Option<(String, i64)>> {
        let row =
            sqlx::query("SELECT leaseholder, running_until FROM poll_lease WHERE job_id = ?1")
                .bind(job_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| (r.get::<String, _>(0), r.get::<i64, _>(1))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    #[tokio::test]
    async fn cursor_roundtrip() {
        let s = PollState::open_in_memory().await.unwrap();
        let cursor = b"history-id-12345".to_vec();
        s.save_tick_ok("job-a", Some(&cursor), 3, 3, 100, now_ms() + 60_000, now_ms())
            .await
            .unwrap();
        let snap = s.load("job-a").await.unwrap().unwrap();
        assert_eq!(snap.cursor.as_deref(), Some(cursor.as_slice()));
        assert_eq!(snap.items_seen_total, 3);
        assert_eq!(snap.items_dispatched_total, 3);
        assert_eq!(snap.consecutive_errors, 0);
        assert_eq!(snap.last_status.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn save_err_increments_consecutive() {
        let s = PollState::open_in_memory().await.unwrap();
        for i in 0..3 {
            s.save_tick_err(
                "job-b",
                "transient",
                &format!("err {i}"),
                now_ms() + 60_000,
                10,
                now_ms(),
                true,
            )
            .await
            .unwrap();
        }
        let snap = s.load("job-b").await.unwrap().unwrap();
        assert_eq!(snap.consecutive_errors, 3);
        assert_eq!(snap.last_status.as_deref(), Some("transient"));
        assert_eq!(snap.last_error.as_deref(), Some("err 2"));
    }

    #[tokio::test]
    async fn ok_resets_consecutive_errors() {
        let s = PollState::open_in_memory().await.unwrap();
        s.save_tick_err("j", "transient", "boom", now_ms(), 0, now_ms(), true)
            .await
            .unwrap();
        s.save_tick_err("j", "transient", "boom", now_ms(), 0, now_ms(), true)
            .await
            .unwrap();
        s.save_tick_ok("j", None, 1, 1, 5, now_ms() + 60_000, now_ms())
            .await
            .unwrap();
        let snap = s.load("j").await.unwrap().unwrap();
        assert_eq!(snap.consecutive_errors, 0);
    }

    #[tokio::test]
    async fn lease_unique_until_expiry() {
        let s = PollState::open_in_memory().await.unwrap();
        let now = now_ms();
        let until = now + 5_000;
        assert!(s.acquire_lease("j", "A", until, now).await.unwrap());
        // Second caller cannot grab it before expiry.
        assert!(!s.acquire_lease("j", "B", now + 6_000, now).await.unwrap());
        // After expiry, takeover succeeds.
        let later = until + 1;
        assert!(s.acquire_lease("j", "B", later + 5_000, later).await.unwrap());
        let (holder, _) = s.lease_holder("j").await.unwrap().unwrap();
        assert_eq!(holder, "B");
    }

    #[tokio::test]
    async fn release_lease_removes_row() {
        let s = PollState::open_in_memory().await.unwrap();
        let now = now_ms();
        s.acquire_lease("j", "A", now + 5_000, now).await.unwrap();
        s.release_lease("j", "A").await.unwrap();
        assert!(s.lease_holder("j").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn release_does_not_steal_others_lease() {
        let s = PollState::open_in_memory().await.unwrap();
        let now = now_ms();
        s.acquire_lease("j", "A", now + 5_000, now).await.unwrap();
        // B tries to release — should be a no-op.
        s.release_lease("j", "B").await.unwrap();
        let (holder, _) = s.lease_holder("j").await.unwrap().unwrap();
        assert_eq!(holder, "A");
    }

    #[tokio::test]
    async fn paused_survives_save() {
        let s = PollState::open_in_memory().await.unwrap();
        s.set_paused("j", true, now_ms()).await.unwrap();
        let snap = s.load("j").await.unwrap().unwrap();
        assert!(snap.paused);
    }

    #[tokio::test]
    async fn reset_cursor_clears_blob_and_errors() {
        let s = PollState::open_in_memory().await.unwrap();
        let cursor = b"old".to_vec();
        s.save_tick_ok("j", Some(&cursor), 1, 1, 10, now_ms(), now_ms())
            .await
            .unwrap();
        s.save_tick_err("j", "transient", "x", now_ms(), 0, now_ms(), true)
            .await
            .unwrap();
        s.reset_cursor("j", now_ms()).await.unwrap();
        let snap = s.load("j").await.unwrap().unwrap();
        assert!(snap.cursor.is_none());
        assert_eq!(snap.consecutive_errors, 0);
        assert!(snap.last_error.is_none());
        assert!(!snap.paused);
    }
}
