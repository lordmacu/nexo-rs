//! Persistent bounce history (Phase 48 follow-up #4).
//!
//! SQLite-backed registry of every `BounceEvent` the inbound worker
//! parses. The `email_send` tool reads this so it can warn the agent
//! when a recipient has hit a permanent failure recently — the
//! warning is advisory, not blocking, since operators may have
//! cleaned up the destination after the bounce.
//!
//! Schema is keyed on `(instance, recipient)`: each new bounce
//! upserts the latest classification + status, increments a count,
//! and bumps `last_seen`. We only store rows with a non-empty
//! recipient — heuristic-detected MAILER-DAEMON bounces without a
//! `Final-Recipient:` field have no key to index on, so they're
//! silently dropped from the store (the wire event still publishes).

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::SqlitePool;

use crate::dsn::{BounceClassification, BounceEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipientStatus {
    pub instance: String,
    pub recipient: String,
    pub classification: BounceClassification,
    pub status_code: Option<String>,
    pub action: Option<String>,
    pub last_seen: i64,
    pub count: i64,
}

#[derive(Clone)]
pub struct BounceStore {
    pool: SqlitePool,
}

impl BounceStore {
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
                    "cannot create email bounce store parent dir: {}",
                    parent.display()
                )
            })?;
        }
        Self::open(&format!("sqlite://{}", path.display())).await
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS email_bounces (
                instance       TEXT NOT NULL,
                recipient      TEXT NOT NULL,
                classification TEXT NOT NULL,
                status_code    TEXT,
                action         TEXT,
                last_seen      INTEGER NOT NULL,
                count          INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (instance, recipient)
            )",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Upsert a single bounce. Drops rows without a recipient (the
    /// composite key requires both fields).
    pub async fn record(&self, event: &BounceEvent) -> Result<()> {
        let Some(recipient) = event.recipient.as_deref() else {
            return Ok(());
        };
        let recipient_norm = recipient.trim().to_ascii_lowercase();
        if recipient_norm.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        let classification = classification_label(event.classification);
        sqlx::query(
            "INSERT INTO email_bounces
                (instance, recipient, classification, status_code, action, last_seen, count)
             VALUES (?, ?, ?, ?, ?, ?, 1)
             ON CONFLICT(instance, recipient) DO UPDATE SET
                classification = excluded.classification,
                status_code    = excluded.status_code,
                action         = excluded.action,
                last_seen      = excluded.last_seen,
                count          = email_bounces.count + 1",
        )
        .bind(&event.instance)
        .bind(&recipient_norm)
        .bind(classification)
        .bind(event.status_code.as_deref())
        .bind(event.action.as_deref())
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get(
        &self,
        instance: &str,
        recipient: &str,
    ) -> Result<Option<RecipientStatus>> {
        let recipient_norm = recipient.trim().to_ascii_lowercase();
        let row = sqlx::query_as::<_, (String, String, String, Option<String>, Option<String>, i64, i64)>(
            "SELECT instance, recipient, classification, status_code, action, last_seen, count
             FROM email_bounces WHERE instance = ? AND recipient = ?",
        )
        .bind(instance)
        .bind(&recipient_norm)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(i, r, c, s, a, ts, n)| RecipientStatus {
            instance: i,
            recipient: r,
            classification: classification_from_label(&c),
            status_code: s,
            action: a,
            last_seen: ts,
            count: n,
        }))
    }
}

fn classification_label(c: BounceClassification) -> &'static str {
    match c {
        BounceClassification::Permanent => "permanent",
        BounceClassification::Transient => "transient",
        BounceClassification::Unknown => "unknown",
    }
}

fn classification_from_label(s: &str) -> BounceClassification {
    match s {
        "permanent" => BounceClassification::Permanent,
        "transient" => BounceClassification::Transient,
        _ => BounceClassification::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(instance: &str, recipient: Option<&str>, c: BounceClassification) -> BounceEvent {
        BounceEvent {
            account_id: format!("{instance}@example.com"),
            instance: instance.to_string(),
            original_message_id: Some("<orig@x>".into()),
            recipient: recipient.map(String::from),
            status_code: Some("5.1.1".into()),
            action: Some("failed".into()),
            reason: Some("user unknown".into()),
            classification: c,
        }
    }

    async fn fresh() -> BounceStore {
        BounceStore::open("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn empty_store_returns_none() {
        let s = fresh().await;
        assert!(s.get("ops", "alice@x").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn record_then_get_round_trips() {
        let s = fresh().await;
        s.record(&ev("ops", Some("alice@x"), BounceClassification::Permanent))
            .await
            .unwrap();
        let r = s.get("ops", "alice@x").await.unwrap().unwrap();
        assert_eq!(r.classification, BounceClassification::Permanent);
        assert_eq!(r.count, 1);
        assert_eq!(r.status_code.as_deref(), Some("5.1.1"));
    }

    #[tokio::test]
    async fn second_bounce_increments_count() {
        let s = fresh().await;
        s.record(&ev("ops", Some("alice@x"), BounceClassification::Transient))
            .await
            .unwrap();
        s.record(&ev("ops", Some("alice@x"), BounceClassification::Permanent))
            .await
            .unwrap();
        let r = s.get("ops", "alice@x").await.unwrap().unwrap();
        assert_eq!(r.count, 2);
        // Latest classification wins.
        assert_eq!(r.classification, BounceClassification::Permanent);
    }

    #[tokio::test]
    async fn missing_recipient_is_silently_dropped() {
        let s = fresh().await;
        s.record(&ev("ops", None, BounceClassification::Unknown))
            .await
            .unwrap();
        // Nothing written; the lookup misses cleanly.
        assert!(s.get("ops", "anything@x").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn recipient_lookup_is_case_insensitive() {
        let s = fresh().await;
        s.record(&ev("ops", Some("Alice@X"), BounceClassification::Permanent))
            .await
            .unwrap();
        let r = s.get("ops", "ALICE@x").await.unwrap().unwrap();
        assert_eq!(r.recipient, "alice@x");
    }

    #[tokio::test]
    async fn isolated_per_instance() {
        let s = fresh().await;
        s.record(&ev("a", Some("x@y"), BounceClassification::Permanent))
            .await
            .unwrap();
        s.record(&ev("b", Some("x@y"), BounceClassification::Transient))
            .await
            .unwrap();
        let a = s.get("a", "x@y").await.unwrap().unwrap();
        let b = s.get("b", "x@y").await.unwrap().unwrap();
        assert_eq!(a.classification, BounceClassification::Permanent);
        assert_eq!(b.classification, BounceClassification::Transient);
    }
}
