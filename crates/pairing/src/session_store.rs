//! Companion session tokens — issued after a successful WS handshake.
//!
//! `PairingSessionStore` persists session tokens in SQLite so the daemon
//! can validate companion requests after restart without forcing re-pairing.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tracing;

use crate::types::PairingError;

pub struct PairingSessionStore {
    pool: SqlitePool,
}

pub struct SessionRow {
    pub profile: String,
    pub device_label: Option<String>,
    pub expires_at: DateTime<Utc>,
}

impl PairingSessionStore {
    pub async fn open(path: &Path) -> Result<Self, PairingError> {
        Self::connect(&path.to_string_lossy(), 4).await
    }

    pub async fn open_memory() -> Result<Self, PairingError> {
        Self::connect(":memory:", 1).await
    }

    async fn connect(path: &str, max_conns: u32) -> Result<Self, PairingError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(opts)
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS pairing_sessions (\
                token        TEXT PRIMARY KEY,\
                profile      TEXT NOT NULL,\
                device_label TEXT,\
                issued_at    INTEGER NOT NULL,\
                expires_at   INTEGER NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        Ok(Self { pool })
    }

    pub async fn insert_session(
        &self,
        token: &str,
        profile: &str,
        device_label: Option<&str>,
        ttl: Duration,
    ) -> Result<(), PairingError> {
        let now = Utc::now();
        let issued_at = now.timestamp();
        let ttl_dur = chrono::Duration::from_std(ttl)
            .map_err(|_| PairingError::Invalid("session ttl out of range"))?;
        let expires_at = (now + ttl_dur).timestamp();
        // Best-effort GC — don't block the insert if cleanup fails.
        if let Err(e) = self.expire_sessions().await {
            tracing::warn!(error = %e, "pairing session GC failed");
        }
        sqlx::query(
            "INSERT OR REPLACE INTO pairing_sessions \
             (token, profile, device_label, issued_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(token)
        .bind(profile)
        .bind(device_label)
        .bind(issued_at)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        Ok(())
    }

    pub async fn lookup_session(&self, token: &str) -> Result<Option<SessionRow>, PairingError> {
        let now = Utc::now().timestamp();
        let row: Option<(String, Option<String>, i64)> = sqlx::query_as(
            "SELECT profile, device_label, expires_at \
             FROM pairing_sessions WHERE token = ?1 AND expires_at >= ?2",
        )
        .bind(token)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        row.map(|(profile, device_label, expires_at_ts)| {
            let expires_at = DateTime::<Utc>::from_timestamp(expires_at_ts, 0).ok_or(
                PairingError::Storage(format!("corrupt expires_at timestamp: {expires_at_ts}")),
            )?;
            Ok(SessionRow {
                profile,
                device_label,
                expires_at,
            })
        })
        .transpose()
    }

    pub async fn expire_sessions(&self) -> Result<usize, PairingError> {
        let now = Utc::now().timestamp();
        let result = sqlx::query("DELETE FROM pairing_sessions WHERE expires_at < ?1")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
        Ok(result.rows_affected() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn insert_and_lookup() {
        let store = PairingSessionStore::open_memory().await.unwrap();
        store
            .insert_session(
                "tok1",
                "companion-v1",
                Some("phone"),
                Duration::from_secs(3600),
            )
            .await
            .unwrap();
        let row = store.lookup_session("tok1").await.unwrap().unwrap();
        assert_eq!(row.profile, "companion-v1");
        assert_eq!(row.device_label.as_deref(), Some("phone"));
    }

    #[tokio::test]
    async fn unknown_token_returns_none() {
        let store = PairingSessionStore::open_memory().await.unwrap();
        assert!(store.lookup_session("nosuchtoken").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_row_not_returned() {
        let store = PairingSessionStore::open_memory().await.unwrap();
        let issued_at = Utc::now().timestamp() - 10;
        let expires_at = Utc::now().timestamp() - 1;
        sqlx::query(
            "INSERT INTO pairing_sessions \
             (token, profile, device_label, issued_at, expires_at) \
             VALUES ('expired_tok', 'companion-v1', NULL, ?1, ?2)",
        )
        .bind(issued_at)
        .bind(expires_at)
        .execute(&store.pool)
        .await
        .unwrap();
        assert!(store.lookup_session("expired_tok").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expire_sessions_clears_old_rows() {
        let store = PairingSessionStore::open_memory().await.unwrap();
        let issued_at = Utc::now().timestamp() - 10;
        let expires_at = Utc::now().timestamp() - 1;
        sqlx::query(
            "INSERT INTO pairing_sessions \
             (token, profile, device_label, issued_at, expires_at) \
             VALUES ('old_tok', 'p', NULL, ?1, ?2)",
        )
        .bind(issued_at)
        .bind(expires_at)
        .execute(&store.pool)
        .await
        .unwrap();
        let deleted = store.expire_sessions().await.unwrap();
        assert_eq!(deleted, 1);
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pairing_sessions WHERE token = 'old_tok'")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
    }
}
