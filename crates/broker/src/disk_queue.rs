use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

use crate::handle::BrokerHandle;
use crate::types::Event;

/// Maximum backpressure sleep applied when the queue is at the hard cap.
const MAX_BACKPRESSURE_MS: u64 = 500;
/// Max retry attempts before a pending event is moved to the dead-letter
/// queue. Tuned for "transient outage" scenarios — a real 3-minute NATS
/// blip with a 10s circuit backoff gives each event ~18 drain cycles
/// before landing in DLQ.
const DEFAULT_MAX_ATTEMPTS: i32 = 3;

pub struct DiskQueue {
    pool: SqlitePool,
    max_pending: usize,
    /// Guard against overlapping drains: reconnect storms can fire two
    /// drains in quick succession; without this they'd both fetch the
    /// same top-100 rows and publish duplicates to NATS.
    drain_running: AtomicBool,
}

impl DiskQueue {
    pub async fn new(path: &str, max_pending: usize) -> anyhow::Result<Self> {
        let url = if path == ":memory:" {
            "sqlite::memory:".to_string()
        } else {
            // Ensure parent dir exists
            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent).await?;
                }
            }
            format!("sqlite:{path}?mode=rwc")
        };

        let pool = SqlitePool::connect(&url).await?;
        Self::migrate(&pool).await?;
        Ok(Self {
            pool,
            max_pending,
            drain_running: AtomicBool::new(false),
        })
    }

    async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS pending_events (
                id          TEXT    NOT NULL PRIMARY KEY,
                topic       TEXT    NOT NULL,
                payload     TEXT    NOT NULL,
                enqueued_at INTEGER NOT NULL,
                attempts    INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS dead_letters (
                id          TEXT    NOT NULL PRIMARY KEY,
                topic       TEXT    NOT NULL,
                payload     TEXT    NOT NULL,
                failed_at   INTEGER NOT NULL,
                reason      TEXT    NOT NULL DEFAULT ''
            );
            "#,
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    pub async fn enqueue(&self, topic: &str, event: &Event) -> anyhow::Result<()> {
        let count = self.pending_count().await?;

        // Backpressure: once the queue passes 50% capacity, slow the producer
        // proportional to how full it is. At the hard cap we pay the full
        // `MAX_BACKPRESSURE_MS` AND drop the oldest entry — the cap is a
        // liveness guard, not a silent truncation.
        let half = self.max_pending / 2;
        if count >= self.max_pending {
            tokio::time::sleep(Duration::from_millis(MAX_BACKPRESSURE_MS)).await;
            sqlx::query(
                "DELETE FROM pending_events WHERE id = (SELECT id FROM pending_events ORDER BY enqueued_at ASC LIMIT 1)"
            )
            .execute(&self.pool)
            .await?;
            tracing::warn!(
                topic,
                pending = count,
                max = self.max_pending,
                "disk queue at hard cap — slept + dropped oldest event"
            );
        } else if count > half {
            let range = (self.max_pending - half).max(1) as u64;
            let over = (count - half) as u64;
            let ms = (over * MAX_BACKPRESSURE_MS) / range;
            if ms > 0 {
                tracing::debug!(topic, pending = count, sleep_ms = ms, "applying backpressure");
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
        }

        let id = event.id.to_string();
        let payload = serde_json::to_string(event)?;
        let now = Self::now_ms();

        sqlx::query(
            "INSERT OR IGNORE INTO pending_events (id, topic, payload, enqueued_at, attempts) VALUES (?, ?, ?, ?, 0)"
        )
        .bind(&id)
        .bind(topic)
        .bind(&payload)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Drain up to 100 pending events. Returns number successfully published.
    pub async fn drain(&self, broker: &(impl BrokerHandle + ?Sized)) -> anyhow::Result<usize> {
        let rows = sqlx::query_as::<_, PendingRow>(
            "SELECT id, topic, payload, attempts FROM pending_events ORDER BY enqueued_at ASC LIMIT 100"
        )
        .fetch_all(&self.pool)
        .await?;

        let mut published = 0usize;
        for row in rows {
            let event: Event = match serde_json::from_str(&row.payload) {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!(id = %row.id, error = %e, "failed to deserialize pending event — moving to DLQ");
                    self.move_to_dlq(&row.id, &row.topic, &row.payload, "deserialize_error").await?;
                    continue;
                }
            };

            match broker.publish(&row.topic, event).await {
                Ok(()) => {
                    sqlx::query("DELETE FROM pending_events WHERE id = ?")
                        .bind(&row.id)
                        .execute(&self.pool)
                        .await?;
                    published += 1;
                }
                Err(e) => {
                    let new_attempts = row.attempts + 1;
                    if new_attempts >= DEFAULT_MAX_ATTEMPTS {
                        tracing::warn!(id = %row.id, topic = %row.topic, "max attempts reached — moving to DLQ");
                        self.move_to_dlq(&row.id, &row.topic, &row.payload, &e.to_string()).await?;
                    } else {
                        sqlx::query("UPDATE pending_events SET attempts = ? WHERE id = ?")
                            .bind(new_attempts)
                            .bind(&row.id)
                            .execute(&self.pool)
                            .await?;
                        tracing::warn!(id = %row.id, attempts = new_attempts, error = %e, "publish failed");
                    }
                }
            }
        }

        Ok(published)
    }

    async fn move_to_dlq(&self, id: &str, topic: &str, payload: &str, reason: &str) -> anyhow::Result<()> {
        let now = Self::now_ms();
        sqlx::query(
            "INSERT OR IGNORE INTO dead_letters (id, topic, payload, failed_at, reason) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(id)
        .bind(topic)
        .bind(payload)
        .bind(now)
        .bind(reason)
        .execute(&self.pool)
        .await?;

        sqlx::query("DELETE FROM pending_events WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    pub async fn pending_count(&self) -> anyhow::Result<usize> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pending_events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0 as usize)
    }

    pub async fn dead_letter_count(&self) -> anyhow::Result<usize> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM dead_letters")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0 as usize)
    }

    pub async fn list_dead_letters(&self, limit: usize) -> anyhow::Result<Vec<DeadLetter>> {
        let rows = sqlx::query_as::<_, DeadLetter>(
            "SELECT id, topic, payload, failed_at, reason FROM dead_letters \
             ORDER BY failed_at DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Move a DLQ entry back into pending with `attempts = 0` so the next
    /// drain retries it. Returns false if the id isn't in the DLQ.
    pub async fn replay_dead_letter(&self, id: &str) -> anyhow::Result<bool> {
        let row = sqlx::query_as::<_, DeadLetter>(
            "SELECT id, topic, payload, failed_at, reason FROM dead_letters WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else { return Ok(false) };

        sqlx::query(
            "INSERT OR REPLACE INTO pending_events (id, topic, payload, enqueued_at, attempts) \
             VALUES (?, ?, ?, ?, 0)",
        )
        .bind(&row.id)
        .bind(&row.topic)
        .bind(&row.payload)
        .bind(Self::now_ms())
        .execute(&self.pool)
        .await?;

        sqlx::query("DELETE FROM dead_letters WHERE id = ?")
            .bind(&row.id)
            .execute(&self.pool)
            .await?;

        Ok(true)
    }

    pub async fn purge_dead_letters(&self) -> anyhow::Result<usize> {
        let result = sqlx::query("DELETE FROM dead_letters")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() as usize)
    }

    /// Drain pending events directly through a NATS client (used on reconnect).
    pub async fn drain_nats(&self, client: &async_nats::Client) -> anyhow::Result<usize> {
        // Overlap guard: if another drain is in progress, skip. The
        // caller (state monitor) triggers a drain on every reconnect;
        // two reconnects 50ms apart would otherwise double-publish
        // every queued event.
        if self
            .drain_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::debug!("drain_nats skipped — another drain in progress");
            return Ok(0);
        }
        // Scope-guard so the flag resets on every exit path.
        struct Guard<'a>(&'a AtomicBool);
        impl<'a> Drop for Guard<'a> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _guard = Guard(&self.drain_running);

        let rows = sqlx::query_as::<_, PendingRow>(
            "SELECT id, topic, payload, attempts FROM pending_events ORDER BY enqueued_at ASC LIMIT 100"
        )
        .fetch_all(&self.pool)
        .await?;

        let mut published = 0usize;
        for row in rows {
            let event: Event = match serde_json::from_str(&row.payload) {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!(id = %row.id, error = %e, "deserialize error in drain_nats — moving to DLQ");
                    self.move_to_dlq(&row.id, &row.topic, &row.payload, "deserialize_error").await?;
                    continue;
                }
            };

            let bytes = bytes::Bytes::from(serde_json::to_vec(&event)?);
            match client.publish(row.topic.clone(), bytes).await {
                Ok(()) => {
                    sqlx::query("DELETE FROM pending_events WHERE id = ?")
                        .bind(&row.id)
                        .execute(&self.pool)
                        .await?;
                    published += 1;
                }
                Err(e) => {
                    let new_attempts = row.attempts + 1;
                    if new_attempts >= DEFAULT_MAX_ATTEMPTS {
                        self.move_to_dlq(&row.id, &row.topic, &row.payload, &e.to_string()).await?;
                    } else {
                        sqlx::query("UPDATE pending_events SET attempts = ? WHERE id = ?")
                            .bind(new_attempts)
                            .bind(&row.id)
                            .execute(&self.pool)
                            .await?;
                    }
                }
            }
        }
        Ok(published)
    }
}

#[derive(sqlx::FromRow)]
struct PendingRow {
    id: String,
    topic: String,
    payload: String,
    attempts: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DeadLetter {
    pub id: String,
    pub topic: String,
    pub payload: String,
    pub failed_at: i64,
    pub reason: String,
}
