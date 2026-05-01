//! Phase 82.10.h.1 — SQLite-backed admin audit writer.
//!
//! Persists [`AdminAuditRow`] across daemon restarts. Append is
//! fail-tolerant (errors log-warn, never propagate to dispatch).
//! Boot-time `sweep_retention()` enforces age + cap limits via
//! the env-var-driven INVENTORY toggles
//! `NEXO_MICROAPP_ADMIN_AUDIT_RETENTION_DAYS` (default 90) and
//! `NEXO_MICROAPP_ADMIN_AUDIT_MAX_ROWS` (default 100_000).
//!
//! DDL is inline + idempotent (`CREATE TABLE IF NOT EXISTS`) so
//! the writer can `open()` against a brand-new path or an
//! already-populated one. Mirrors the `runMigrations()`
//! forward-only pattern.

use std::path::Path;
use std::str::FromStr;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use super::audit::{AdminAuditRow, AdminAuditWriter};
#[cfg(test)]
use super::audit::AdminAuditResult;

/// SQLite-backed `AdminAuditWriter`. Production daemons construct
/// one at boot and feed it to
/// `AdminRpcDispatcher::with_audit_writer`.
#[derive(Debug, Clone)]
pub struct SqliteAdminAuditWriter {
    pool: SqlitePool,
}

impl SqliteAdminAuditWriter {
    /// Open or create the audit DB at `path`. Idempotent — the
    /// inline DDL runs on every boot. Pool kept small (2 conns)
    /// since audit writes are infrequent and append-only.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let path_str = path.display().to_string();
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path_str}"))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(opts)
            .await?;
        sqlx::query("PRAGMA journal_mode=WAL").execute(&pool).await.ok();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS microapp_admin_audit (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                microapp_id TEXT NOT NULL,
                method TEXT NOT NULL,
                capability TEXT NOT NULL,
                args_hash TEXT NOT NULL,
                started_at_ms INTEGER NOT NULL,
                result TEXT NOT NULL CHECK(result IN ('ok','error','denied')),
                error_code INTEGER,
                duration_ms INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_microapp_admin_audit_microapp
                ON microapp_admin_audit(microapp_id, started_at_ms DESC)",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_microapp_admin_audit_method
                ON microapp_admin_audit(method, started_at_ms DESC)",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    /// In-memory variant for tests. No filesystem path; pool drops
    /// content on `Self::Drop`.
    pub async fn open_memory() -> anyhow::Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?;
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(opts)
            .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS microapp_admin_audit (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                microapp_id TEXT NOT NULL,
                method TEXT NOT NULL,
                capability TEXT NOT NULL,
                args_hash TEXT NOT NULL,
                started_at_ms INTEGER NOT NULL,
                result TEXT NOT NULL CHECK(result IN ('ok','error','denied')),
                error_code INTEGER,
                duration_ms INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    /// Boot-time retention sweep. Deletes rows older than
    /// `retention_days` AND drops the oldest beyond `max_rows`.
    /// Returns total rows deleted. Errors propagate — caller
    /// (boot supervisor) logs them; admin dispatch never depends
    /// on this fn.
    pub async fn sweep_retention(
        &self,
        retention_days: u64,
        max_rows: usize,
    ) -> anyhow::Result<usize> {
        let mut deleted = 0usize;

        // 1. Time-based delete.
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let cutoff_ms = now_ms.saturating_sub(retention_days * 86_400 * 1000);
        let res = sqlx::query("DELETE FROM microapp_admin_audit WHERE started_at_ms < ?")
            .bind(cutoff_ms as i64)
            .execute(&self.pool)
            .await?;
        deleted += res.rows_affected() as usize;

        // 2. Cap-based delete.
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM microapp_admin_audit")
                .fetch_one(&self.pool)
                .await?;
        if (total as usize) > max_rows {
            let excess = (total as usize) - max_rows;
            let res = sqlx::query(
                "DELETE FROM microapp_admin_audit WHERE id IN (
                    SELECT id FROM microapp_admin_audit
                    ORDER BY started_at_ms ASC LIMIT ?
                )",
            )
            .bind(excess as i64)
            .execute(&self.pool)
            .await?;
            deleted += res.rows_affected() as usize;
        }
        Ok(deleted)
    }

    /// Test-only — read all rows. Production callers use the CLI
    /// tail command (Step 2).
    #[cfg(test)]
    async fn all_rows(&self) -> anyhow::Result<Vec<AdminAuditRow>> {
        let rows: Vec<(String, String, String, String, i64, String, Option<i32>, i64)> =
            sqlx::query_as(
                "SELECT microapp_id, method, capability, args_hash, started_at_ms, \
                 result, error_code, duration_ms FROM microapp_admin_audit \
                 ORDER BY started_at_ms ASC",
            )
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(microapp_id, method, capability, args_hash, started_at_ms, result, _err, duration_ms)| {
                    AdminAuditRow {
                        microapp_id,
                        method,
                        capability,
                        args_hash,
                        started_at_ms: started_at_ms as u64,
                        result: match result.as_str() {
                            "ok" => AdminAuditResult::Ok,
                            "denied" => AdminAuditResult::Denied,
                            _ => AdminAuditResult::Error,
                        },
                        duration_ms: duration_ms as u64,
                    }
                },
            )
            .collect())
    }
}

#[async_trait]
impl AdminAuditWriter for SqliteAdminAuditWriter {
    async fn append(&self, row: AdminAuditRow) {
        let result_str = row.result.as_str();
        let res = sqlx::query(
            "INSERT INTO microapp_admin_audit
                (microapp_id, method, capability, args_hash, started_at_ms,
                 result, error_code, duration_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.microapp_id)
        .bind(&row.method)
        .bind(&row.capability)
        .bind(&row.args_hash)
        .bind(row.started_at_ms as i64)
        .bind(result_str)
        .bind::<Option<i32>>(None)
        .bind(row.duration_ms as i64)
        .execute(&self.pool)
        .await;
        if let Err(e) = res {
            tracing::warn!(
                microapp_id = %row.microapp_id,
                method = %row.method,
                error = %e,
                "admin audit append failed; row dropped",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row(microapp_id: &str, started_at_ms: u64) -> AdminAuditRow {
        AdminAuditRow {
            microapp_id: microapp_id.into(),
            method: "nexo/admin/agents/list".into(),
            capability: "agents_crud".into(),
            args_hash: "abc".into(),
            started_at_ms,
            result: AdminAuditResult::Ok,
            duration_ms: 5,
        }
    }

    #[tokio::test]
    async fn sqlite_writer_creates_table_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.db");
        let _w1 = SqliteAdminAuditWriter::open(&path).await.unwrap();
        // Re-open same path — DDL should not error.
        let _w2 = SqliteAdminAuditWriter::open(&path).await.unwrap();
    }

    #[tokio::test]
    async fn sqlite_writer_appends_row_and_reads_back() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        writer.append(sample_row("agent-creator", 1_000_000)).await;
        let rows = writer.all_rows().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].microapp_id, "agent-creator");
        assert_eq!(rows[0].method, "nexo/admin/agents/list");
        assert_eq!(rows[0].args_hash, "abc");
    }

    #[tokio::test]
    async fn sqlite_writer_swallows_errors_on_closed_pool() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        writer.pool.close().await;
        // Should NOT panic — the warn-log path swallows.
        writer.append(sample_row("a", 1)).await;
    }

    #[tokio::test]
    async fn sweep_retention_deletes_old_rows_by_age() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let day_ms: u64 = 86_400 * 1000;
        // 100d: older than 60d retention → deleted
        // 30d: within retention → kept
        // 1d: within retention → kept
        writer.append(sample_row("a", now_ms - 100 * day_ms)).await;
        writer.append(sample_row("a", now_ms - 30 * day_ms)).await;
        writer.append(sample_row("a", now_ms - day_ms)).await;
        let deleted = writer.sweep_retention(60, 1_000_000).await.unwrap();
        assert_eq!(deleted, 1, "only the 100-day-old row should age out");
        let rows = writer.all_rows().await.unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn sweep_retention_caps_to_max_rows_drops_oldest() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        for i in 0..10u64 {
            writer.append(sample_row("a", now_ms - (10 - i) * 1000)).await;
        }
        // Retention generous (none deleted by age); max_rows=3 →
        // 7 oldest deleted.
        let deleted = writer.sweep_retention(365, 3).await.unwrap();
        assert_eq!(deleted, 7);
        let rows = writer.all_rows().await.unwrap();
        assert_eq!(rows.len(), 3);
        // Most recent retained.
        for row in &rows {
            assert!(row.started_at_ms >= now_ms - 3 * 1000);
        }
    }
}
