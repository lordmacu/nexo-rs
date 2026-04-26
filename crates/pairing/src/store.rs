//! SQLite-backed pairing storage.
//!
//! Two tables in one DB file:
//! - `pairing_pending` — short-lived (TTL 60 min) requests issued via
//!   the DM challenge flow. Pruned eagerly on insert.
//! - `pairing_allow_from` — durable per-channel allowlist. Soft-delete
//!   on revoke (`revoked_at` timestamp) so the operator can audit.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::code;
use crate::types::{AllowedSender, ApprovedRequest, PairingError, PendingRequest, UpsertOutcome};

const PENDING_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_PENDING_PER_ACCOUNT: usize = 3;

pub struct PairingStore {
    pool: SqlitePool,
}

impl PairingStore {
    pub async fn open(path: &str) -> Result<Self, PairingError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        // SQLite's `:memory:` is per-connection, so pin to one
        // connection in tests; file-backed paths use the normal pool.
        let max_conns = if path == ":memory:" { 1 } else { 4 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(opts)
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS pairing_pending (\
                channel    TEXT NOT NULL,\
                account_id TEXT NOT NULL,\
                sender_id  TEXT NOT NULL,\
                code       TEXT NOT NULL,\
                created_at INTEGER NOT NULL,\
                meta_json  TEXT NOT NULL DEFAULT '{}',\
                PRIMARY KEY (channel, account_id, sender_id)\
            )",
        )
        .execute(&pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_pairing_pending_code ON pairing_pending(code)")
            .execute(&pool)
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS pairing_allow_from (\
                channel       TEXT NOT NULL,\
                account_id    TEXT NOT NULL,\
                sender_id     TEXT NOT NULL,\
                approved_at   INTEGER NOT NULL,\
                approved_via  TEXT NOT NULL DEFAULT 'cli',\
                revoked_at    INTEGER,\
                PRIMARY KEY (channel, account_id, sender_id)\
            )",
        )
        .execute(&pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        Ok(Self { pool })
    }

    pub async fn open_memory() -> Result<Self, PairingError> {
        Self::open(":memory:").await
    }

    /// Insert (or refresh `created_at` on) a pending request. Enforces
    /// TTL prune + max-pending per (channel, account). Returns the
    /// active code (existing or new) and `created=true` when this
    /// call inserted a fresh row.
    pub async fn upsert_pending(
        &self,
        channel: &str,
        account_id: &str,
        sender_id: &str,
        meta: serde_json::Value,
    ) -> Result<UpsertOutcome, PairingError> {
        // Prune expired everywhere first (cheap, O(rows)).
        self.purge_expired().await?;

        // Already pending for this sender? Refresh `created_at` and
        // return the existing code so repeated DMs don't keep
        // generating new codes (matches OpenClaw's `lastSeenAt`
        // behaviour).
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT code FROM pairing_pending WHERE channel = ? AND account_id = ? AND sender_id = ?",
        )
        .bind(channel)
        .bind(account_id)
        .bind(sender_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        if let Some(code) = existing {
            return Ok(UpsertOutcome {
                code,
                created: false,
            });
        }

        // Enforce per-(channel, account) cap before inserting.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pairing_pending WHERE channel = ? AND account_id = ?",
        )
        .bind(channel)
        .bind(account_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        if count as usize >= MAX_PENDING_PER_ACCOUNT {
            return Err(PairingError::MaxPending {
                channel: channel.into(),
                account_id: account_id.into(),
            });
        }

        // Generate a code that does not collide with any *active* code
        // anywhere in the table.
        let active_codes: Vec<String> = sqlx::query_scalar("SELECT code FROM pairing_pending")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
        let set: HashSet<String> = active_codes.into_iter().collect();
        let code = code::generate_unique(&set).map_err(PairingError::Invalid)?;

        let now = Utc::now().timestamp();
        let meta_json =
            serde_json::to_string(&meta).map_err(|e| PairingError::Storage(e.to_string()))?;
        sqlx::query(
            "INSERT INTO pairing_pending(channel, account_id, sender_id, code, created_at, meta_json) VALUES(?, ?, ?, ?, ?, ?)",
        )
        .bind(channel)
        .bind(account_id)
        .bind(sender_id)
        .bind(&code)
        .bind(now)
        .bind(meta_json)
        .execute(&self.pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        crate::telemetry::inc_requests_pending(channel);
        Ok(UpsertOutcome {
            code,
            created: true,
        })
    }

    pub async fn list_pending(
        &self,
        channel: Option<&str>,
    ) -> Result<Vec<PendingRequest>, PairingError> {
        let rows: Vec<(String, String, String, String, i64, String)> = if let Some(c) = channel {
            sqlx::query_as(
                "SELECT channel, account_id, sender_id, code, created_at, meta_json FROM pairing_pending WHERE channel = ? ORDER BY created_at",
            )
            .bind(c)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                "SELECT channel, account_id, sender_id, code, created_at, meta_json FROM pairing_pending ORDER BY created_at",
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for (channel, account_id, sender_id, code, created_at, meta_json) in rows {
            let meta: serde_json::Value =
                serde_json::from_str(&meta_json).unwrap_or(serde_json::Value::Null);
            let created_at =
                DateTime::<Utc>::from_timestamp(created_at, 0).unwrap_or_else(Utc::now);
            out.push(PendingRequest {
                channel,
                account_id,
                sender_id,
                code,
                created_at,
                meta,
            });
        }
        Ok(out)
    }

    /// Dump every row from `pairing_allow_from`. `include_revoked=false`
    /// hides soft-deleted rows; `true` returns them too with
    /// `revoked_at` populated. `channel` filters when `Some(_)`. The
    /// `nexo pair list --all` operator surface relies on this to make
    /// seeded senders visible (the legacy `list_pending` only shows
    /// in-flight challenges, which left operators unable to confirm a
    /// `pair seed` actually landed).
    pub async fn list_allow(
        &self,
        channel: Option<&str>,
        include_revoked: bool,
    ) -> Result<Vec<AllowedSender>, PairingError> {
        let mut sql = String::from(
            "SELECT channel, account_id, sender_id, approved_at, approved_via, revoked_at \
             FROM pairing_allow_from",
        );
        let mut clauses: Vec<&str> = Vec::new();
        if !include_revoked {
            clauses.push("revoked_at IS NULL");
        }
        if channel.is_some() {
            clauses.push("channel = ?");
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY channel, account_id, sender_id");
        let rows: Vec<(String, String, String, i64, String, Option<i64>)> = if let Some(c) = channel
        {
            sqlx::query_as(&sql)
                .bind(c)
                .fetch_all(&self.pool)
                .await
        } else {
            sqlx::query_as(&sql).fetch_all(&self.pool).await
        }
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for (channel, account_id, sender_id, approved_at, approved_via, revoked_at) in rows {
            let approved_at =
                DateTime::<Utc>::from_timestamp(approved_at, 0).unwrap_or_else(Utc::now);
            let revoked_at =
                revoked_at.and_then(|t| DateTime::<Utc>::from_timestamp(t, 0));
            out.push(AllowedSender {
                channel,
                account_id,
                sender_id,
                approved_at,
                approved_via,
                revoked_at,
            });
        }
        Ok(out)
    }

    /// Approve a pending request by its code. Moves the row from
    /// `pairing_pending` into `pairing_allow_from` atomically.
    pub async fn approve(&self, code_value: &str) -> Result<ApprovedRequest, PairingError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
        let row: Option<(String, String, String, i64)> = sqlx::query_as(
            "SELECT channel, account_id, sender_id, created_at FROM pairing_pending WHERE code = ?",
        )
        .bind(code_value)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        let Some((channel, account_id, sender_id, created_at)) = row else {
            crate::telemetry::inc_approvals("", "not_found");
            return Err(PairingError::UnknownCode);
        };
        // Reject if expired (the prune may not have run since insert).
        let age = Utc::now().timestamp() - created_at;
        if age > PENDING_TTL.as_secs() as i64 {
            crate::telemetry::inc_approvals(&channel, "expired");
            crate::telemetry::add_codes_expired(1);
            return Err(PairingError::Expired);
        }
        sqlx::query(
            "INSERT INTO pairing_allow_from(channel, account_id, sender_id, approved_at, approved_via, revoked_at) VALUES(?, ?, ?, ?, 'cli', NULL) ON CONFLICT(channel, account_id, sender_id) DO UPDATE SET revoked_at = NULL, approved_at = excluded.approved_at, approved_via = excluded.approved_via",
        )
        .bind(&channel)
        .bind(&account_id)
        .bind(&sender_id)
        .bind(Utc::now().timestamp())
        .execute(&mut *tx)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        sqlx::query("DELETE FROM pairing_pending WHERE code = ?")
            .bind(code_value)
            .execute(&mut *tx)
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
        crate::telemetry::inc_approvals(&channel, "ok");
        crate::telemetry::dec_requests_pending(&channel);
        Ok(ApprovedRequest {
            channel,
            account_id,
            sender_id,
            approved_at: Utc::now(),
        })
    }

    /// Soft-delete by setting `revoked_at`. The row stays for audit.
    /// Returns `true` if a row was updated (caller decides whether to
    /// surface "already revoked / not found").
    pub async fn revoke(&self, channel: &str, sender_id: &str) -> Result<bool, PairingError> {
        let res = sqlx::query(
            "UPDATE pairing_allow_from SET revoked_at = ? WHERE channel = ? AND sender_id = ? AND revoked_at IS NULL",
        )
        .bind(Utc::now().timestamp())
        .bind(channel)
        .bind(sender_id)
        .execute(&self.pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn is_allowed(
        &self,
        channel: &str,
        account_id: &str,
        sender_id: &str,
    ) -> Result<bool, PairingError> {
        let row: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM pairing_allow_from WHERE channel = ? AND account_id = ? AND sender_id = ? AND revoked_at IS NULL",
        )
        .bind(channel)
        .bind(account_id)
        .bind(sender_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        Ok(row.is_some())
    }

    /// Bulk insert (idempotent) — preload allow-from from a known
    /// list of senders, e.g. when migrating from a non-pairing setup.
    pub async fn seed(
        &self,
        channel: &str,
        account_id: &str,
        sender_ids: &[String],
    ) -> Result<usize, PairingError> {
        let mut count = 0usize;
        let now = Utc::now().timestamp();
        for sender in sender_ids {
            let res = sqlx::query(
                "INSERT INTO pairing_allow_from(channel, account_id, sender_id, approved_at, approved_via, revoked_at) VALUES(?, ?, ?, ?, 'seed', NULL) ON CONFLICT(channel, account_id, sender_id) DO UPDATE SET revoked_at = NULL",
            )
            .bind(channel)
            .bind(account_id)
            .bind(sender)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
            count += res.rows_affected() as usize;
        }
        Ok(count)
    }

    /// Test-only access to the underlying pool. Lets integration tests
    /// in this crate backdate rows or assert raw state without
    /// duplicating the schema setup. Hidden from rustdoc; do not
    /// rely on this from production callers.
    #[doc(hidden)]
    pub fn pool_for_test(&self) -> &SqlitePool {
        &self.pool
    }

    /// Resync the `pairing_requests_pending` gauge from the database.
    /// Call after process restart (the gauge is in-memory state and
    /// resets to 0, so without a refresh it under-reports until the
    /// next `upsert_pending`). Channels that had a value but no longer
    /// have any pending rows are clamped to 0 to avoid ghost gauges.
    pub async fn refresh_pending_gauge(&self) -> Result<(), PairingError> {
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT channel, COUNT(*) FROM pairing_pending GROUP BY channel")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| PairingError::Storage(e.to_string()))?;
        let live: std::collections::HashSet<String> = rows.iter().map(|(c, _)| c.clone()).collect();
        for prior in crate::telemetry::pending_channels() {
            if !live.contains(&prior) {
                crate::telemetry::set_requests_pending(&prior, 0);
            }
        }
        for (channel, count) in rows {
            crate::telemetry::set_requests_pending(&channel, count);
        }
        Ok(())
    }

    pub async fn purge_expired(&self) -> Result<u64, PairingError> {
        let cutoff = Utc::now().timestamp() - PENDING_TTL.as_secs() as i64;
        // Count rows about to die per-channel so we can keep the
        // pending gauge in sync without a follow-up query / refresh.
        let by_channel: Vec<(String, i64)> = sqlx::query_as(
            "SELECT channel, COUNT(*) FROM pairing_pending WHERE created_at < ? GROUP BY channel",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PairingError::Storage(e.to_string()))?;
        let res = sqlx::query("DELETE FROM pairing_pending WHERE created_at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|e| PairingError::Storage(e.to_string()))?;
        let n = res.rows_affected();
        if n > 0 {
            crate::telemetry::add_codes_expired(n);
            for (channel, count) in by_channel {
                crate::telemetry::sub_requests_pending(&channel, count);
            }
        }
        Ok(n)
    }
}
