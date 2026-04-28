//! `SqliteAuditLogStore` — production audit-log backend.
//!
//! Mirrors `crates/agent-registry/src/turn_log.rs:100-260` (Phase
//! 72): WAL + synchronous=NORMAL, idempotent INSERT OR REPLACE on
//! the primary key, async `tail` with hard cap. The MCP audit
//! schema differs from per-turn (different columns + indexes) but
//! the I/O pattern is identical — keep them aligned so a single
//! operator playbook covers both.

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use super::store::{AuditError, AuditLogStore};
use super::types::{AuditFilter, AuditOutcome, AuditRow};

/// Hard cap on `tail` results so a sloppy filter can't drag the
/// whole DB into memory. Mirrors `turn_log.rs::TAIL_HARD_CAP`.
const TAIL_HARD_CAP: usize = 1000;

pub struct SqliteAuditLogStore {
    pool: SqlitePool,
}

impl SqliteAuditLogStore {
    /// Open or create the DB file. WAL + synchronous=NORMAL for
    /// the in-tree pattern. Pool size 4 (1 for `:memory:`).
    pub async fn open(path: &str) -> Result<Self, AuditError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let max_conns = if path == ":memory:" { 1 } else { 4 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(opts)
            .await
            .map_err(|e| AuditError::Backend(e.to_string()))?;
        if path != ":memory:" {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&pool)
                .await
                .map_err(|e| AuditError::Backend(e.to_string()))?;
            sqlx::query("PRAGMA synchronous = NORMAL")
                .execute(&pool)
                .await
                .map_err(|e| AuditError::Backend(e.to_string()))?;
        }
        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_memory() -> Result<Self, AuditError> {
        Self::open(":memory:").await
    }

    async fn migrate(pool: &SqlitePool) -> Result<(), AuditError> {
        // 18-column schema mirroring `AuditRow`. `outcome` is the
        // string label so a human running `sqlite3` can read the
        // table without decoding integers.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS mcp_call_audit (\
                call_id           TEXT PRIMARY KEY,\
                request_id        TEXT,\
                session_id        TEXT,\
                tenant            TEXT NOT NULL,\
                subject           TEXT,\
                auth_method       TEXT NOT NULL,\
                method            TEXT NOT NULL,\
                tool_name         TEXT,\
                args_hash         TEXT,\
                args_size_bytes   INTEGER NOT NULL,\
                started_at_ms     INTEGER NOT NULL,\
                completed_at_ms   INTEGER,\
                duration_ms       INTEGER,\
                outcome           TEXT NOT NULL,\
                error_code        INTEGER,\
                error_message     TEXT,\
                result_size_bytes INTEGER,\
                retry_after_ms    INTEGER\
            )",
        )
        .execute(pool)
        .await
        .map_err(|e| AuditError::Backend(e.to_string()))?;
        // Three indexes per the spec: tenant+started, tool+outcome,
        // started_at alone (for prune scans + time-range queries).
        for stmt in [
            "CREATE INDEX IF NOT EXISTS idx_audit_tenant_started \
                ON mcp_call_audit(tenant, started_at_ms)",
            "CREATE INDEX IF NOT EXISTS idx_audit_tool_outcome \
                ON mcp_call_audit(tool_name, outcome)",
            "CREATE INDEX IF NOT EXISTS idx_audit_started \
                ON mcp_call_audit(started_at_ms)",
        ] {
            sqlx::query(stmt)
                .execute(pool)
                .await
                .map_err(|e| AuditError::Backend(e.to_string()))?;
        }
        Ok(())
    }
}

fn outcome_label(o: AuditOutcome) -> &'static str {
    o.as_label()
}

fn parse_outcome(s: &str) -> AuditOutcome {
    match s {
        "ok" => AuditOutcome::Ok,
        "error" => AuditOutcome::Error,
        "cancelled" => AuditOutcome::Cancelled,
        "timeout" => AuditOutcome::Timeout,
        "rate_limited" => AuditOutcome::RateLimited,
        "denied" => AuditOutcome::Denied,
        "panicked" => AuditOutcome::Panicked,
        // Unknown label = future-tolerant fallback. Logged once
        // by the caller if surprising.
        _ => AuditOutcome::Error,
    }
}

#[async_trait]
impl AuditLogStore for SqliteAuditLogStore {
    async fn append(&self, rows: Vec<AuditRow>) -> Result<(), AuditError> {
        // Single transaction so the batch is atomic — partial
        // success on a 50-row flush is worse than all-or-nothing.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AuditError::Backend(e.to_string()))?;
        for row in rows {
            sqlx::query(
                "INSERT INTO mcp_call_audit \
                     (call_id, request_id, session_id, tenant, subject, auth_method, \
                      method, tool_name, args_hash, args_size_bytes, started_at_ms, \
                      completed_at_ms, duration_ms, outcome, error_code, error_message, \
                      result_size_bytes, retry_after_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                         ?15, ?16, ?17, ?18) \
                 ON CONFLICT(call_id) DO UPDATE SET \
                     request_id        = excluded.request_id, \
                     session_id        = excluded.session_id, \
                     tenant            = excluded.tenant, \
                     subject           = excluded.subject, \
                     auth_method       = excluded.auth_method, \
                     method            = excluded.method, \
                     tool_name         = excluded.tool_name, \
                     args_hash         = excluded.args_hash, \
                     args_size_bytes   = excluded.args_size_bytes, \
                     started_at_ms     = excluded.started_at_ms, \
                     completed_at_ms   = excluded.completed_at_ms, \
                     duration_ms       = excluded.duration_ms, \
                     outcome           = excluded.outcome, \
                     error_code        = excluded.error_code, \
                     error_message     = excluded.error_message, \
                     result_size_bytes = excluded.result_size_bytes, \
                     retry_after_ms    = excluded.retry_after_ms",
            )
            .bind(&row.call_id)
            .bind(&row.request_id)
            .bind(&row.session_id)
            .bind(&row.tenant)
            .bind(&row.subject)
            .bind(&row.auth_method)
            .bind(&row.method)
            .bind(&row.tool_name)
            .bind(&row.args_hash)
            .bind(row.args_size_bytes)
            .bind(row.started_at_ms)
            .bind(row.completed_at_ms)
            .bind(row.duration_ms)
            .bind(outcome_label(row.outcome))
            .bind(row.error_code)
            .bind(&row.error_message)
            .bind(row.result_size_bytes)
            .bind(row.retry_after_ms)
            .execute(&mut *tx)
            .await
            .map_err(|e| AuditError::Backend(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| AuditError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn tail(&self, filter: &AuditFilter, limit: usize) -> Result<Vec<AuditRow>, AuditError> {
        let limit = if limit == 0 {
            TAIL_HARD_CAP
        } else {
            limit.min(TAIL_HARD_CAP)
        };
        // Build a parameterised WHERE with optional clauses. Every
        // parameter is bound (no string interpolation) so SQL
        // injection isn't a concern.
        let mut sql = String::from(
            "SELECT call_id, request_id, session_id, tenant, subject, auth_method, \
                method, tool_name, args_hash, args_size_bytes, started_at_ms, \
                completed_at_ms, duration_ms, outcome, error_code, error_message, \
                result_size_bytes, retry_after_ms \
             FROM mcp_call_audit",
        );
        let mut clauses: Vec<&str> = Vec::new();
        if filter.tenant.is_some() {
            clauses.push("tenant = ?");
        }
        if filter.tool_name.is_some() {
            clauses.push("tool_name = ?");
        }
        if filter.outcome.is_some() {
            clauses.push("outcome = ?");
        }
        if filter.since_ms.is_some() {
            clauses.push("started_at_ms >= ?");
        }
        if filter.until_ms.is_some() {
            clauses.push("started_at_ms < ?");
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY started_at_ms DESC LIMIT ?");

        let mut q = sqlx::query_as::<_, AuditRowSqlx>(&sql);
        if let Some(t) = filter.tenant.as_deref() {
            q = q.bind(t);
        }
        if let Some(t) = filter.tool_name.as_deref() {
            q = q.bind(t);
        }
        if let Some(o) = filter.outcome {
            q = q.bind(outcome_label(o));
        }
        if let Some(s) = filter.since_ms {
            q = q.bind(s);
        }
        if let Some(u) = filter.until_ms {
            q = q.bind(u);
        }
        q = q.bind(limit as i64);
        let rows: Vec<AuditRowSqlx> = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| AuditError::Backend(e.to_string()))?;
        Ok(rows.into_iter().map(AuditRowSqlx::into_row).collect())
    }

    async fn count(&self, filter: &AuditFilter) -> Result<u64, AuditError> {
        let mut sql = String::from("SELECT COUNT(*) FROM mcp_call_audit");
        let mut clauses: Vec<&str> = Vec::new();
        if filter.tenant.is_some() {
            clauses.push("tenant = ?");
        }
        if filter.tool_name.is_some() {
            clauses.push("tool_name = ?");
        }
        if filter.outcome.is_some() {
            clauses.push("outcome = ?");
        }
        if filter.since_ms.is_some() {
            clauses.push("started_at_ms >= ?");
        }
        if filter.until_ms.is_some() {
            clauses.push("started_at_ms < ?");
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        let mut q = sqlx::query_scalar::<_, i64>(&sql);
        if let Some(t) = filter.tenant.as_deref() {
            q = q.bind(t);
        }
        if let Some(t) = filter.tool_name.as_deref() {
            q = q.bind(t);
        }
        if let Some(o) = filter.outcome {
            q = q.bind(outcome_label(o));
        }
        if let Some(s) = filter.since_ms {
            q = q.bind(s);
        }
        if let Some(u) = filter.until_ms {
            q = q.bind(u);
        }
        let n: i64 = q
            .fetch_one(&self.pool)
            .await
            .map_err(|e| AuditError::Backend(e.to_string()))?;
        Ok(n.max(0) as u64)
    }

    async fn prune_older_than(&self, cutoff_ms: i64) -> Result<u64, AuditError> {
        let res = sqlx::query("DELETE FROM mcp_call_audit WHERE started_at_ms < ?1")
            .bind(cutoff_ms)
            .execute(&self.pool)
            .await
            .map_err(|e| AuditError::Backend(e.to_string()))?;
        Ok(res.rows_affected())
    }
}

// `sqlx` derive needs a struct with all-`Decode` fields. We
// reconstruct `AuditRow` from this in `into_row`.
#[derive(sqlx::FromRow)]
struct AuditRowSqlx {
    call_id: String,
    request_id: Option<String>,
    session_id: Option<String>,
    tenant: String,
    subject: Option<String>,
    auth_method: String,
    method: String,
    tool_name: Option<String>,
    args_hash: Option<String>,
    args_size_bytes: i64,
    started_at_ms: i64,
    completed_at_ms: Option<i64>,
    duration_ms: Option<i64>,
    outcome: String,
    error_code: Option<i64>, // SQLite stores INTEGER as i64
    error_message: Option<String>,
    result_size_bytes: Option<i64>,
    retry_after_ms: Option<i64>,
}

impl AuditRowSqlx {
    fn into_row(self) -> AuditRow {
        AuditRow {
            call_id: self.call_id,
            request_id: self.request_id,
            session_id: self.session_id,
            tenant: self.tenant,
            subject: self.subject,
            auth_method: self.auth_method,
            method: self.method,
            tool_name: self.tool_name,
            args_hash: self.args_hash,
            args_size_bytes: self.args_size_bytes,
            started_at_ms: self.started_at_ms,
            completed_at_ms: self.completed_at_ms,
            duration_ms: self.duration_ms,
            outcome: parse_outcome(&self.outcome),
            error_code: self.error_code.map(|v| v as i32),
            error_message: self.error_message,
            result_size_bytes: self.result_size_bytes,
            retry_after_ms: self.retry_after_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(call_id: &str, tenant: &str, started_ms: i64, outcome: AuditOutcome) -> AuditRow {
        AuditRow {
            call_id: call_id.into(),
            request_id: Some("req-1".into()),
            session_id: Some("sess-1".into()),
            tenant: tenant.into(),
            subject: Some("subj".into()),
            auth_method: "static_token".into(),
            method: "tools/call".into(),
            tool_name: Some("echo".into()),
            args_hash: Some("a1b2c3".into()),
            args_size_bytes: 42,
            started_at_ms: started_ms,
            completed_at_ms: Some(started_ms + 10),
            duration_ms: Some(10),
            outcome,
            error_code: None,
            error_message: None,
            result_size_bytes: Some(128),
            retry_after_ms: None,
        }
    }

    #[tokio::test]
    async fn open_memory_creates_schema() {
        let store = SqliteAuditLogStore::open_memory().await.unwrap();
        // `count` on an empty table works — proves schema exists.
        let n = store.count(&AuditFilter::default()).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn append_then_tail_round_trip() {
        let store = SqliteAuditLogStore::open_memory().await.unwrap();
        store
            .append(vec![row("a", "t1", 100, AuditOutcome::Ok)])
            .await
            .unwrap();
        let out = store.tail(&AuditFilter::default(), 10).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].call_id, "a");
        assert_eq!(out[0].tenant, "t1");
        assert_eq!(out[0].outcome, AuditOutcome::Ok);
        assert_eq!(out[0].args_hash.as_deref(), Some("a1b2c3"));
    }

    #[tokio::test]
    async fn idempotent_append_overwrites_on_call_id() {
        let store = SqliteAuditLogStore::open_memory().await.unwrap();
        let mut r1 = row("a", "t", 100, AuditOutcome::Ok);
        r1.error_message = Some("first".into());
        store.append(vec![r1.clone()]).await.unwrap();
        let mut r2 = r1.clone();
        r2.error_message = Some("second".into());
        store.append(vec![r2]).await.unwrap();
        let out = store.tail(&AuditFilter::default(), 10).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].error_message.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn tail_filters_by_tenant_and_outcome() {
        let store = SqliteAuditLogStore::open_memory().await.unwrap();
        store
            .append(vec![
                row("a", "t1", 100, AuditOutcome::Ok),
                row("b", "t2", 200, AuditOutcome::Timeout),
                row("c", "t1", 300, AuditOutcome::Timeout),
            ])
            .await
            .unwrap();
        let f = AuditFilter {
            tenant: Some("t1".into()),
            outcome: Some(AuditOutcome::Timeout),
            ..Default::default()
        };
        let out = store.tail(&f, 10).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].call_id, "c");
    }

    #[tokio::test]
    async fn tail_is_newest_first() {
        let store = SqliteAuditLogStore::open_memory().await.unwrap();
        store
            .append(vec![
                row("a", "t", 100, AuditOutcome::Ok),
                row("b", "t", 200, AuditOutcome::Ok),
                row("c", "t", 50, AuditOutcome::Ok),
            ])
            .await
            .unwrap();
        let out = store.tail(&AuditFilter::default(), 10).await.unwrap();
        assert_eq!(out[0].call_id, "b");
        assert_eq!(out[1].call_id, "a");
        assert_eq!(out[2].call_id, "c");
    }

    #[tokio::test]
    async fn time_range_filter() {
        let store = SqliteAuditLogStore::open_memory().await.unwrap();
        store
            .append(vec![
                row("a", "t", 100, AuditOutcome::Ok),
                row("b", "t", 200, AuditOutcome::Ok),
                row("c", "t", 300, AuditOutcome::Ok),
            ])
            .await
            .unwrap();
        let f = AuditFilter {
            since_ms: Some(150),
            until_ms: Some(250),
            ..Default::default()
        };
        let out = store.tail(&f, 10).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].call_id, "b");
    }

    #[tokio::test]
    async fn prune_older_than_drops_only_old_rows() {
        let store = SqliteAuditLogStore::open_memory().await.unwrap();
        store
            .append(vec![
                row("a", "t", 100, AuditOutcome::Ok),
                row("b", "t", 200, AuditOutcome::Ok),
                row("c", "t", 300, AuditOutcome::Ok),
            ])
            .await
            .unwrap();
        let n = store.prune_older_than(250).await.unwrap();
        assert_eq!(n, 2);
        assert_eq!(store.count(&AuditFilter::default()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn outcome_label_round_trips_through_storage() {
        let store = SqliteAuditLogStore::open_memory().await.unwrap();
        for o in [
            AuditOutcome::Ok,
            AuditOutcome::Error,
            AuditOutcome::Cancelled,
            AuditOutcome::Timeout,
            AuditOutcome::RateLimited,
            AuditOutcome::Denied,
            AuditOutcome::Panicked,
        ] {
            let id = format!("call-{}", o.as_label());
            store.append(vec![row(&id, "t", 100, o)]).await.unwrap();
        }
        let out = store.tail(&AuditFilter::default(), 100).await.unwrap();
        assert_eq!(out.len(), 7);
        let outcomes: std::collections::HashSet<AuditOutcome> =
            out.iter().map(|r| r.outcome).collect();
        assert_eq!(outcomes.len(), 7);
    }

    #[tokio::test]
    async fn count_matches_filter() {
        let store = SqliteAuditLogStore::open_memory().await.unwrap();
        store
            .append(vec![
                row("a", "t1", 100, AuditOutcome::Ok),
                row("b", "t1", 200, AuditOutcome::Ok),
                row("c", "t2", 300, AuditOutcome::Ok),
            ])
            .await
            .unwrap();
        let f = AuditFilter {
            tenant: Some("t1".into()),
            ..Default::default()
        };
        assert_eq!(store.count(&f).await.unwrap(), 2);
    }
}
