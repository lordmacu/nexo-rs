//! Attachment ref-counting + retention GC (Phase 48 follow-up #10).
//!
//! The MIME parser de-dupes attachments by SHA-256 across every
//! account: `<attachments_dir>/<sha256>` exists once no matter how
//! many accounts received the same file. That's storage-efficient
//! but means nothing reclaims an attachment once nobody references
//! it any more.
//!
//! This module ships the cheapest correct ref-counting we can get
//! away with: a sqlx-sqlite table that records every `record(sha256)`
//! the inbound worker makes, with a `last_seen` column. A periodic
//! GC sweep deletes both the file and the table row when `last_seen`
//! is older than the configured retention.
//!
//! Operators who want infinite retention set
//! `attachment_retention_days: 0`; the GC task short-circuits.

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::SqlitePool;

#[derive(Clone)]
pub struct AttachmentStore {
    pool: SqlitePool,
}

impl AttachmentStore {
    pub async fn open(db_path: &str) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(db_path)
            .with_context(|| format!("invalid sqlite path: {db_path}"))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePool::connect_with(opts).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    pub async fn open_path(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "cannot create attachment store parent dir: {}",
                    parent.display()
                )
            })?;
        }
        Self::open(&format!("sqlite://{}", path.display())).await
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS email_attachments (
                sha256     TEXT PRIMARY KEY,
                first_seen INTEGER NOT NULL,
                last_seen  INTEGER NOT NULL,
                count      INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert or bump. The first reference sets `first_seen`;
    /// subsequent references just bump `last_seen` + `count`.
    pub async fn record(&self, sha256: &str) -> Result<()> {
        if sha256.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO email_attachments (sha256, first_seen, last_seen, count)
             VALUES (?, ?, ?, 1)
             ON CONFLICT(sha256) DO UPDATE SET
                last_seen = excluded.last_seen,
                count     = email_attachments.count + 1",
        )
        .bind(sha256)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Reap rows whose `last_seen` is older than `retention_seconds`
    /// before now. Deletes both the table row and the on-disk file
    /// at `<attachments_dir>/<sha256>`. Files that vanish out from
    /// under us (manual cleanup, fs error) just log and continue —
    /// the row still gets purged. Returns the count of files
    /// removed.
    pub async fn gc(&self, attachments_dir: &Path, retention_seconds: i64) -> Result<usize> {
        if retention_seconds <= 0 {
            return Ok(0);
        }
        let cutoff = chrono::Utc::now().timestamp() - retention_seconds;
        let stale: Vec<(String,)> = sqlx::query_as(
            "SELECT sha256 FROM email_attachments WHERE last_seen < ?",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        let mut removed = 0usize;
        for (sha256,) in &stale {
            let path = attachments_dir.join(sha256);
            match tokio::fs::remove_file(&path).await {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Already gone (operator clean, fs error). Drop
                    // the row anyway so we don't keep retrying.
                }
                Err(e) => {
                    tracing::warn!(
                        target: "plugin.email",
                        sha256 = %sha256,
                        path = %path.display(),
                        error = %e,
                        "attachment GC: file remove failed (continuing)"
                    );
                }
            }
        }
        if !stale.is_empty() {
            // Single bulk delete — much cheaper than one per row.
            sqlx::query("DELETE FROM email_attachments WHERE last_seen < ?")
                .bind(cutoff)
                .execute(&self.pool)
                .await?;
        }
        Ok(removed)
    }

    pub async fn count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM email_attachments")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    pub async fn last_seen(&self, sha256: &str) -> Result<Option<i64>> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT last_seen FROM email_attachments WHERE sha256 = ?",
        )
        .bind(sha256)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(t,)| t))
    }
}

/// Convenience for tests that want to backdate a row so GC takes it
/// without sleeping.
#[cfg(test)]
pub async fn backdate_for_test(
    store: &AttachmentStore,
    sha256: &str,
    last_seen: i64,
) -> Result<()> {
    sqlx::query("UPDATE email_attachments SET last_seen = ? WHERE sha256 = ?")
        .bind(last_seen)
        .bind(sha256)
        .execute(&store.pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn fresh() -> AttachmentStore {
        AttachmentStore::open("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn record_then_count() {
        let s = fresh().await;
        s.record("abc").await.unwrap();
        s.record("abc").await.unwrap();
        s.record("def").await.unwrap();
        assert_eq!(s.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn record_bumps_last_seen() {
        let s = fresh().await;
        s.record("abc").await.unwrap();
        let t1 = s.last_seen("abc").await.unwrap().unwrap();
        backdate_for_test(&s, "abc", t1 - 1000).await.unwrap();
        let t2 = s.last_seen("abc").await.unwrap().unwrap();
        assert_eq!(t2, t1 - 1000);
        s.record("abc").await.unwrap();
        let t3 = s.last_seen("abc").await.unwrap().unwrap();
        assert!(t3 >= t1, "last_seen must be bumped, got {t3} vs {t1}");
    }

    #[tokio::test]
    async fn empty_sha_is_dropped_silently() {
        let s = fresh().await;
        s.record("").await.unwrap();
        assert_eq!(s.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn gc_zero_retention_is_a_noop() {
        let dir = tempdir().unwrap();
        let s = fresh().await;
        s.record("abc").await.unwrap();
        let n = s.gc(dir.path(), 0).await.unwrap();
        assert_eq!(n, 0);
        assert_eq!(s.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn gc_removes_stale_files_and_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("abc");
        tokio::fs::write(&path, b"payload").await.unwrap();
        let s = fresh().await;
        s.record("abc").await.unwrap();
        // Backdate by 200 days.
        let now = chrono::Utc::now().timestamp();
        backdate_for_test(&s, "abc", now - 200 * 86_400).await.unwrap();
        let removed = s.gc(dir.path(), 90 * 86_400).await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(s.count().await.unwrap(), 0);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn gc_keeps_fresh_rows_untouched() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("abc");
        tokio::fs::write(&path, b"payload").await.unwrap();
        let s = fresh().await;
        s.record("abc").await.unwrap();
        // Don't backdate — it's fresh.
        let removed = s.gc(dir.path(), 90 * 86_400).await.unwrap();
        assert_eq!(removed, 0);
        assert!(path.exists());
        assert_eq!(s.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn gc_swallows_missing_file() {
        // The row references a sha256 whose file already vanished.
        // GC drops the row anyway so we don't keep retrying.
        let dir = tempdir().unwrap();
        let s = fresh().await;
        s.record("abc").await.unwrap();
        let now = chrono::Utc::now().timestamp();
        backdate_for_test(&s, "abc", now - 200 * 86_400).await.unwrap();
        let removed = s.gc(dir.path(), 90 * 86_400).await.unwrap();
        // File was never written — `removed` counts only the
        // successful unlinks, but the row is gone regardless.
        assert_eq!(removed, 0);
        assert_eq!(s.count().await.unwrap(), 0);
    }
}
