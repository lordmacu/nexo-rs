//! Persistent IMAP UID cursor per account (Phase 48.3).
//!
//! Stores the last `(uid_validity, last_uid)` we successfully published
//! so a daemon restart resumes from the right place. If the server
//! reports a different `UIDVALIDITY` (admin recreated the mailbox), we
//! reset `last_uid` to 0 — every existing message is treated as new.
//!
//! Backed by sqlx-sqlite. `:memory:` works for tests; production sets
//! the path to `<data_dir>/email/cursor.db`.

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::SqlitePool;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UidCursor {
    pub uid_validity: u32,
    pub last_uid: u32,
    pub updated_at: i64,
}

#[derive(Clone)]
pub struct CursorStore {
    pool: SqlitePool,
}

impl CursorStore {
    pub async fn open(db_path: &str) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(db_path)
            .with_context(|| format!("invalid sqlite path: {db_path}"))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // Audit follow-up G — WAL + `synchronous = NORMAL` is
            // crash-safe (fsync on checkpoint, not on every commit)
            // and avoids the per-message fsync that would otherwise
            // bottleneck cursor updates under email storms.
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);
        let pool = SqlitePool::connect_with(opts).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    pub async fn open_path(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("cannot create email cursor parent dir: {}", parent.display())
            })?;
        }
        Self::open(&format!("sqlite://{}", path.display())).await
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS email_uid_cursor (
                account_id   TEXT PRIMARY KEY,
                uid_validity INTEGER NOT NULL,
                last_uid     INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get(&self, account_id: &str) -> Result<Option<UidCursor>> {
        let row = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT uid_validity, last_uid, updated_at FROM email_uid_cursor WHERE account_id = ?",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(v, u, t)| UidCursor {
            uid_validity: v as u32,
            last_uid: u as u32,
            updated_at: t,
        }))
    }

    pub async fn set(&self, account_id: &str, c: &UidCursor) -> Result<()> {
        sqlx::query(
            "INSERT INTO email_uid_cursor (account_id, uid_validity, last_uid, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(account_id) DO UPDATE SET
                uid_validity = excluded.uid_validity,
                last_uid     = excluded.last_uid,
                updated_at   = excluded.updated_at",
        )
        .bind(account_id)
        .bind(c.uid_validity as i64)
        .bind(c.last_uid as i64)
        .bind(c.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// If the stored cursor's `uid_validity` differs from `new_validity`
    /// (or there's no stored cursor at all), persist a fresh cursor with
    /// `last_uid = 0`. Returns the resulting cursor either way.
    pub async fn reset_if_validity_changed(
        &self,
        account_id: &str,
        new_validity: u32,
    ) -> Result<UidCursor> {
        let now = chrono::Utc::now().timestamp();
        match self.get(account_id).await? {
            Some(existing) if existing.uid_validity == new_validity => Ok(existing),
            _ => {
                let fresh = UidCursor {
                    uid_validity: new_validity,
                    last_uid: 0,
                    updated_at: now,
                };
                self.set(account_id, &fresh).await?;
                Ok(fresh)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem() -> CursorStore {
        CursorStore::open("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn empty_store_returns_none() {
        let s = mem().await;
        assert!(s.get("ops").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn set_then_get_round_trips() {
        let s = mem().await;
        let c = UidCursor {
            uid_validity: 42,
            last_uid: 100,
            updated_at: 1_700_000_000,
        };
        s.set("ops", &c).await.unwrap();
        assert_eq!(s.get("ops").await.unwrap().unwrap(), c);
    }

    #[tokio::test]
    async fn upsert_overwrites() {
        let s = mem().await;
        let c1 = UidCursor {
            uid_validity: 42,
            last_uid: 100,
            updated_at: 1,
        };
        let c2 = UidCursor {
            uid_validity: 42,
            last_uid: 200,
            updated_at: 2,
        };
        s.set("ops", &c1).await.unwrap();
        s.set("ops", &c2).await.unwrap();
        assert_eq!(s.get("ops").await.unwrap().unwrap().last_uid, 200);
    }

    #[tokio::test]
    async fn reset_keeps_cursor_when_validity_unchanged() {
        let s = mem().await;
        let c = UidCursor {
            uid_validity: 42,
            last_uid: 100,
            updated_at: 1,
        };
        s.set("ops", &c).await.unwrap();
        let kept = s.reset_if_validity_changed("ops", 42).await.unwrap();
        assert_eq!(kept.last_uid, 100);
    }

    #[tokio::test]
    async fn reset_resets_when_validity_changes() {
        let s = mem().await;
        let c = UidCursor {
            uid_validity: 42,
            last_uid: 100,
            updated_at: 1,
        };
        s.set("ops", &c).await.unwrap();
        let fresh = s.reset_if_validity_changed("ops", 99).await.unwrap();
        assert_eq!(fresh.uid_validity, 99);
        assert_eq!(fresh.last_uid, 0);
    }

    #[tokio::test]
    async fn reset_creates_when_absent() {
        let s = mem().await;
        let fresh = s.reset_if_validity_changed("ops", 7).await.unwrap();
        assert_eq!(fresh.uid_validity, 7);
        assert_eq!(fresh.last_uid, 0);
        assert!(s.get("ops").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn isolated_per_account() {
        let s = mem().await;
        let a = UidCursor {
            uid_validity: 1,
            last_uid: 10,
            updated_at: 0,
        };
        let b = UidCursor {
            uid_validity: 1,
            last_uid: 20,
            updated_at: 0,
        };
        s.set("a", &a).await.unwrap();
        s.set("b", &b).await.unwrap();
        assert_eq!(s.get("a").await.unwrap().unwrap().last_uid, 10);
        assert_eq!(s.get("b").await.unwrap().unwrap().last_uid, 20);
    }
}
