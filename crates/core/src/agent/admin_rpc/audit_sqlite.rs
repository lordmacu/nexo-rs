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

use serde::{Deserialize, Serialize};

use super::audit::{AdminAuditResult, AdminAuditRow, AdminAuditWriter};

/// Phase 82.10.h.2 — filter shape for `SqliteAdminAuditWriter::tail`.
/// The `nexo microapp admin audit tail` CLI subcommand maps its
/// flags to this struct (CLI wire-up itself is deferred to
/// 82.10.h.b alongside the broader main.rs production wiring).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditTailFilter {
    /// Restrict to a single microapp.
    pub microapp_id: Option<String>,
    /// Restrict to one JSON-RPC method.
    pub method: Option<String>,
    /// Restrict to one outcome (`ok` / `error` / `denied`).
    pub result: Option<AdminAuditResult>,
    /// Lower-bound timestamp (epoch ms). Use
    /// `chrono::Utc::now().timestamp_millis() - duration_ms` for
    /// human-friendly windows.
    pub since_ms: Option<u64>,
    /// Max rows to return. Default `50` if 0.
    pub limit: usize,
    /// Phase 83.8.12.7 — restrict to a single tenant scope. Rows
    /// stamped from non tenant-scoped calls (`tenant_id IS NULL`)
    /// are excluded when this filter is `Some`. Use `None` (the
    /// default) to leave the tail un-scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

impl AuditTailFilter {
    /// Convenience constructor mirroring CLI defaults.
    pub fn new() -> Self {
        Self {
            limit: 50,
            ..Default::default()
        }
    }
}

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
        Self::run_ddl(&pool).await?;
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
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_microapp_admin_audit_tenant
                ON microapp_admin_audit(tenant_id, started_at_ms DESC)",
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
        Self::run_ddl(&pool).await?;
        Ok(Self { pool })
    }

    /// Phase 83.8.12.7 — DDL bootstrap. Creates the audit table
    /// from scratch with the full current schema; for pre-83.8.12.7
    /// DBs that were created without `tenant_id`, ALTER adds the
    /// column idempotently (the "duplicate column name" error is
    /// the green path on already-migrated DBs and is suppressed).
    async fn run_ddl(pool: &SqlitePool) -> anyhow::Result<()> {
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
                duration_ms INTEGER NOT NULL,
                tenant_id TEXT
            )",
        )
        .execute(pool)
        .await?;
        // Forward-only migration for DBs created before 83.8.12.7.
        // SQLite raises "duplicate column name" if the column
        // already exists — that's the success case.
        if let Err(e) =
            sqlx::query("ALTER TABLE microapp_admin_audit ADD COLUMN tenant_id TEXT")
                .execute(pool)
                .await
        {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(e.into());
            }
        }
        Ok(())
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

    /// Phase 82.10.h.2 — tail recent audit rows with filter
    /// support. Library-level query the `nexo microapp admin
    /// audit tail` CLI subcommand will call once main.rs becomes
    /// buildable (deferred to 82.10.h.b — main.rs has unrelated
    /// in-progress refactors blocking the binary build today).
    pub async fn tail(&self, filter: &AuditTailFilter) -> anyhow::Result<Vec<AdminAuditRow>> {
        let mut sql = String::from(
            "SELECT microapp_id, method, capability, args_hash, started_at_ms, \
             result, error_code, duration_ms, tenant_id \
             FROM microapp_admin_audit WHERE 1=1",
        );
        let mut binds: Vec<String> = Vec::new();
        if let Some(id) = &filter.microapp_id {
            sql.push_str(" AND microapp_id = ?");
            binds.push(id.clone());
        }
        if let Some(method) = &filter.method {
            sql.push_str(" AND method = ?");
            binds.push(method.clone());
        }
        if let Some(result) = &filter.result {
            sql.push_str(" AND result = ?");
            binds.push(result.as_str().to_string());
        }
        if let Some(tenant) = &filter.tenant_id {
            sql.push_str(" AND tenant_id = ?");
            binds.push(tenant.clone());
        }
        let mut int_binds: Vec<i64> = Vec::new();
        if let Some(since_ms) = filter.since_ms {
            sql.push_str(" AND started_at_ms >= ?");
            int_binds.push(since_ms as i64);
        }
        sql.push_str(" ORDER BY started_at_ms DESC LIMIT ?");
        int_binds.push(filter.limit.max(1) as i64);

        let mut q = sqlx::query_as::<
            _,
            (String, String, String, String, i64, String, Option<i32>, i64, Option<String>),
        >(&sql);
        for b in &binds {
            q = q.bind(b);
        }
        for b in &int_binds {
            q = q.bind(*b);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    microapp_id,
                    method,
                    capability,
                    args_hash,
                    started_at_ms,
                    result,
                    _err,
                    duration_ms,
                    tenant_id,
                )| AdminAuditRow {
                    microapp_id,
                    method,
                    capability,
                    args_hash,
                    started_at_ms: started_at_ms as u64,
                    result: AdminAuditResult::from_str(&result),
                    duration_ms: duration_ms as u64,
                    tenant_id,
                },
            )
            .collect())
    }

    /// Phase 83.8.12.7 — convenience tail bound to a single tenant.
    /// Equivalent to `tail()` with `tenant_id = Some(tenant)`,
    /// `since_ms`, and `limit` set; other filters left empty.
    /// Used by the `nexo/admin/audit/tail_for_tenant` future
    /// CLI/RPC subcommand and by SaaS billing pipelines.
    pub async fn tail_for_tenant(
        &self,
        tenant_id: &str,
        since_ms: Option<u64>,
        limit: usize,
    ) -> anyhow::Result<Vec<AdminAuditRow>> {
        self.tail(&AuditTailFilter {
            tenant_id: Some(tenant_id.to_string()),
            since_ms,
            limit: limit.max(1),
            ..Default::default()
        })
        .await
    }

    /// Test-only — read all rows.
    #[cfg(test)]
    pub(crate) async fn all_rows(&self) -> anyhow::Result<Vec<AdminAuditRow>> {
        let rows: Vec<(
            String,
            String,
            String,
            String,
            i64,
            String,
            Option<i32>,
            i64,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT microapp_id, method, capability, args_hash, started_at_ms, \
             result, error_code, duration_ms, tenant_id FROM microapp_admin_audit \
             ORDER BY started_at_ms ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    microapp_id,
                    method,
                    capability,
                    args_hash,
                    started_at_ms,
                    result,
                    _err,
                    duration_ms,
                    tenant_id,
                )| AdminAuditRow {
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
                    tenant_id,
                },
            )
            .collect())
    }
}

/// Phase 82.10.h.2 — render audit rows as a fixed-width text
/// table for the future `nexo microapp admin audit tail` CLI.
/// Columns: `started_at` (ISO-8601 UTC) · `microapp` · `method` ·
/// `result` · `dur_ms` · `args_hash[..8]`. Uses a stable column
/// order so operators can grep / awk the output.
pub fn format_rows_as_table(rows: &[AdminAuditRow]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    writeln!(
        out,
        "{:<24}  {:<20}  {:<40}  {:<7}  {:>7}  {:<10}",
        "started_at", "microapp", "method", "result", "dur_ms", "args[..8]",
    )
    .ok();
    writeln!(out, "{}", "-".repeat(24 + 2 + 20 + 2 + 40 + 2 + 7 + 2 + 7 + 2 + 10)).ok();
    for row in rows {
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(row.started_at_ms as i64)
            .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_else(|| row.started_at_ms.to_string());
        let hash_short: String = row.args_hash.chars().take(8).collect();
        writeln!(
            out,
            "{:<24}  {:<20.20}  {:<40.40}  {:<7}  {:>7}  {:<10}",
            ts,
            row.microapp_id,
            row.method,
            row.result.as_str(),
            row.duration_ms,
            hash_short,
        )
        .ok();
    }
    out
}

/// Phase 82.10.h.2 — render audit rows as a JSON array for
/// machine-readable consumption (`--format json`). Pretty-prints
/// for human review; pipelines that need NDJSON can stream
/// `serde_json::to_string(&row)` per row instead.
pub fn format_rows_as_json(rows: &[AdminAuditRow]) -> String {
    serde_json::to_string_pretty(rows).unwrap_or_else(|_| "[]".into())
}

#[async_trait]
impl AdminAuditWriter for SqliteAdminAuditWriter {
    async fn append(&self, row: AdminAuditRow) {
        let result_str = row.result.as_str();
        let res = sqlx::query(
            "INSERT INTO microapp_admin_audit
                (microapp_id, method, capability, args_hash, started_at_ms,
                 result, error_code, duration_ms, tenant_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.microapp_id)
        .bind(&row.method)
        .bind(&row.capability)
        .bind(&row.args_hash)
        .bind(row.started_at_ms as i64)
        .bind(result_str)
        .bind::<Option<i32>>(None)
        .bind(row.duration_ms as i64)
        .bind(row.tenant_id.as_deref())
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
            tenant_id: None,
        }
    }

    fn sample_row_with_tenant(
        microapp_id: &str,
        started_at_ms: u64,
        tenant_id: &str,
    ) -> AdminAuditRow {
        AdminAuditRow {
            tenant_id: Some(tenant_id.into()),
            ..sample_row(microapp_id, started_at_ms)
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
    async fn tail_filters_by_microapp_id_and_orders_desc() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        writer.append(sample_row("a", 1_000)).await;
        writer.append(sample_row("b", 2_000)).await;
        writer.append(sample_row("a", 3_000)).await;
        let rows = writer
            .tail(&AuditTailFilter {
                microapp_id: Some("a".into()),
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].started_at_ms, 3_000, "newest first");
        assert_eq!(rows[1].started_at_ms, 1_000);
    }

    #[tokio::test]
    async fn tail_filters_by_method_and_result_and_since() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        let mut row1 = sample_row("a", 1_000);
        row1.method = "nexo/admin/agents/list".into();
        row1.result = AdminAuditResult::Ok;
        let mut row2 = sample_row("a", 2_000);
        row2.method = "nexo/admin/agents/list".into();
        row2.result = AdminAuditResult::Denied;
        let mut row3 = sample_row("a", 3_000);
        row3.method = "nexo/admin/credentials/register".into();
        row3.result = AdminAuditResult::Ok;
        writer.append(row1).await;
        writer.append(row2).await;
        writer.append(row3).await;

        let rows = writer
            .tail(&AuditTailFilter {
                method: Some("nexo/admin/agents/list".into()),
                result: Some(AdminAuditResult::Denied),
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].started_at_ms, 2_000);

        let recent = writer
            .tail(&AuditTailFilter {
                since_ms: Some(2_500),
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].started_at_ms, 3_000);
    }

    #[tokio::test]
    async fn tail_respects_limit() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        for i in 0..10 {
            writer.append(sample_row("a", i * 100)).await;
        }
        let rows = writer
            .tail(&AuditTailFilter {
                limit: 3,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn format_table_includes_header_and_rows() {
        let rows = vec![AdminAuditRow {
            microapp_id: "agent-creator".into(),
            method: "nexo/admin/agents/list".into(),
            capability: "agents_crud".into(),
            args_hash: "abcdef0123456789".into(),
            started_at_ms: 1_700_000_000_000,
            result: AdminAuditResult::Ok,
            duration_ms: 12,
            tenant_id: None,
        }];
        let out = format_rows_as_table(&rows);
        assert!(out.contains("started_at"), "header present");
        assert!(out.contains("microapp"));
        assert!(out.contains("agent-creator"));
        assert!(out.contains("nexo/admin/agents/list"));
        assert!(out.contains("ok"));
        assert!(out.contains("abcdef01"), "hash truncated to 8 chars");
        assert!(!out.contains("0123456789"), "full hash should NOT appear");
    }

    #[test]
    fn format_json_round_trips() {
        let rows = vec![AdminAuditRow {
            microapp_id: "a".into(),
            method: "nexo/admin/echo".into(),
            capability: "echo".into(),
            args_hash: "h".into(),
            started_at_ms: 42,
            result: AdminAuditResult::Denied,
            duration_ms: 1,
            tenant_id: None,
        }];
        let json = format_rows_as_json(&rows);
        let back: Vec<AdminAuditRow> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rows);
    }

    #[tokio::test]
    async fn tenant_id_round_trips_through_insert_and_read() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        writer
            .append(sample_row_with_tenant("a", 1_000, "acme"))
            .await;
        writer.append(sample_row("a", 2_000)).await;
        let rows = writer.all_rows().await.unwrap();
        assert_eq!(rows.len(), 2);
        let tenant_row = rows.iter().find(|r| r.started_at_ms == 1_000).unwrap();
        assert_eq!(tenant_row.tenant_id.as_deref(), Some("acme"));
        let null_row = rows.iter().find(|r| r.started_at_ms == 2_000).unwrap();
        assert_eq!(null_row.tenant_id, None);
    }

    #[tokio::test]
    async fn tail_filters_by_tenant_id_excludes_null_rows() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        writer
            .append(sample_row_with_tenant("a", 1_000, "acme"))
            .await;
        writer
            .append(sample_row_with_tenant("a", 2_000, "globex"))
            .await;
        writer.append(sample_row("a", 3_000)).await;
        let rows = writer
            .tail(&AuditTailFilter {
                tenant_id: Some("acme".into()),
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tenant_id.as_deref(), Some("acme"));
        assert_eq!(rows[0].started_at_ms, 1_000);
    }

    #[tokio::test]
    async fn tail_for_tenant_convenience_matches_explicit_filter() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        writer
            .append(sample_row_with_tenant("a", 1_000, "acme"))
            .await;
        writer
            .append(sample_row_with_tenant("a", 2_000, "acme"))
            .await;
        writer
            .append(sample_row_with_tenant("a", 3_000, "globex"))
            .await;
        let rows = writer
            .tail_for_tenant("acme", None, 50)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        // newest first
        assert_eq!(rows[0].started_at_ms, 2_000);
        assert_eq!(rows[1].started_at_ms, 1_000);
        for r in &rows {
            assert_eq!(r.tenant_id.as_deref(), Some("acme"));
        }
    }

    #[tokio::test]
    async fn tail_for_tenant_combines_with_since_ms() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        writer
            .append(sample_row_with_tenant("a", 1_000, "acme"))
            .await;
        writer
            .append(sample_row_with_tenant("a", 5_000, "acme"))
            .await;
        let rows = writer
            .tail_for_tenant("acme", Some(2_500), 50)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].started_at_ms, 5_000);
    }

    #[tokio::test]
    async fn tail_for_tenant_clamps_limit_floor_to_one() {
        let writer = SqliteAdminAuditWriter::open_memory().await.unwrap();
        writer
            .append(sample_row_with_tenant("a", 1_000, "acme"))
            .await;
        let rows = writer.tail_for_tenant("acme", None, 0).await.unwrap();
        assert_eq!(rows.len(), 1, "limit=0 should be clamped to 1, not return empty");
    }

    #[tokio::test]
    async fn ddl_idempotent_when_tenant_id_already_present() {
        // Open + close + re-open the same DB to exercise the
        // ALTER TABLE duplicate-column suppression path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.db");
        let w1 = SqliteAdminAuditWriter::open(&path).await.unwrap();
        w1.append(sample_row_with_tenant("a", 1, "acme")).await;
        drop(w1);
        let w2 = SqliteAdminAuditWriter::open(&path).await.unwrap();
        let rows = w2.all_rows().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tenant_id.as_deref(), Some("acme"));
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
