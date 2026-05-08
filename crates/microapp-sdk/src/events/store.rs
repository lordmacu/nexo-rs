//! `EventStore<T>` — generic SQLite append-log for microapp event
//! firehoses.
//!
//! Lifted from `agent-creator-microapp::firehose::store` with the
//! event type abstracted to any `T: EventMetadata + Serialize +
//! DeserializeOwned`. Schema, indexes, query shape, and the
//! two-phase retention sweep are unchanged so consumers porting
//! over keep their existing on-disk databases.
//!
//! # Schema (created on `open` / `open_memory`)
//!
//! ```sql
//! CREATE TABLE <table> (
//!     id           INTEGER PRIMARY KEY AUTOINCREMENT,
//!     kind         TEXT NOT NULL,
//!     agent_id     TEXT NOT NULL,
//!     tenant_id    TEXT,
//!     at_ms        INTEGER NOT NULL,
//!     payload_json TEXT NOT NULL
//! );
//! CREATE INDEX <table>_agent  ON <table>(agent_id, at_ms DESC);
//! CREATE INDEX <table>_tenant ON <table>(tenant_id, at_ms DESC);
//! CREATE INDEX <table>_kind   ON <table>(kind, at_ms DESC);
//! ```
//!
//! Table name is supplied at `open` time so two stores can coexist
//! in the same SQLite file (e.g. a primary firehose + a side
//! channel) without colliding.

use std::marker::PhantomData;
use std::path::Path;
use std::str::FromStr;

use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use super::metadata::EventMetadata;

/// Default `LIMIT` applied when [`ListFilter::limit`] is `0`. Picked
/// to match the agent-creator firehose UI which paginates 50 at a
/// time.
pub const DEFAULT_LIST_LIMIT: usize = 50;

/// Hard cap on `LIMIT`. Protects the SQLite read from accidental
/// `?limit=1000000` queries.
pub const MAX_LIST_LIMIT: usize = 500;

/// Default table name used when callers don't override on
/// [`EventStore::open`]. Matches the legacy
/// agent-creator-microapp firehose schema so existing on-disk
/// databases keep working after the extraction.
pub const DEFAULT_TABLE: &str = "firehose_events";

/// Read-side filter for [`EventStore::list`]. All fields are
/// `Option`; `None` means "don't constrain on this column".
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    /// Match `agent_id` exactly.
    pub agent_id: Option<String>,
    /// Match the `kind` discriminator exactly. See
    /// [`EventMetadata::kind`] for what gets stored.
    pub kind: Option<String>,
    /// Match `tenant_id` exactly. NULL `tenant_id` rows never match
    /// a non-`None` filter.
    pub tenant_id: Option<String>,
    /// Lower bound on `at_ms` (inclusive). Combine with the
    /// natural `ORDER BY at_ms DESC` to paginate "events newer
    /// than X".
    pub since_ms: Option<u64>,
    /// Row cap. `0` falls back to [`DEFAULT_LIST_LIMIT`]; values
    /// above [`MAX_LIST_LIMIT`] clamp down.
    pub limit: usize,
}

/// Generic SQLite event log. Cheap to clone — wraps an
/// `Arc<SqlitePool>` internally via `sqlx`.
#[derive(Debug, Clone)]
pub struct EventStore<T> {
    pool: SqlitePool,
    table: String,
    _phantom: PhantomData<fn() -> T>,
}

impl<T> EventStore<T>
where
    T: EventMetadata + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Expose the underlying `SqlitePool` so siblings (e.g. a
    /// per-conversation toggle store) can share the same SQLite
    /// file without standing up a second pool. WAL mode lets
    /// readers proceed while the firehose listener is appending.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Open / create the SQLite file at `path` and run the schema
    /// migrations. `table` is the table name — pass
    /// [`DEFAULT_TABLE`] to keep the legacy
    /// agent-creator-microapp shape.
    pub async fn open(path: &Path, table: &str) -> super::Result<Self> {
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
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&pool)
            .await
            .ok();
        Self::run_ddl(&pool, table).await?;
        Ok(Self {
            pool,
            table: table.to_string(),
            _phantom: PhantomData,
        })
    }

    /// In-memory variant for tests. Pool capacity is 1 so the
    /// `sqlite::memory:` instance survives across calls (each new
    /// connection opens a fresh database otherwise).
    pub async fn open_memory(table: &str) -> super::Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::run_ddl(&pool, table).await?;
        Ok(Self {
            pool,
            table: table.to_string(),
            _phantom: PhantomData,
        })
    }

    async fn run_ddl(pool: &SqlitePool, table: &str) -> super::Result<()> {
        // Identifier is interpolated into the SQL because sqlx
        // doesn't parameterise table names. Validate so we can't
        // accidentally inject — table names are operator-supplied
        // at boot, not runtime input, but defence-in-depth is free.
        validate_table_ident(table)?;
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS {table} (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                kind         TEXT NOT NULL,
                agent_id     TEXT NOT NULL,
                tenant_id    TEXT,
                at_ms        INTEGER NOT NULL,
                payload_json TEXT NOT NULL
            )"
        ))
        .execute(pool)
        .await?;
        sqlx::query(&format!(
            "CREATE INDEX IF NOT EXISTS idx_{table}_agent
                ON {table}(agent_id, at_ms DESC)"
        ))
        .execute(pool)
        .await?;
        sqlx::query(&format!(
            "CREATE INDEX IF NOT EXISTS idx_{table}_tenant
                ON {table}(tenant_id, at_ms DESC)"
        ))
        .execute(pool)
        .await?;
        sqlx::query(&format!(
            "CREATE INDEX IF NOT EXISTS idx_{table}_kind
                ON {table}(kind, at_ms DESC)"
        ))
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Append one event. `kind` / `agent_id` / `tenant_id` /
    /// `at_ms` come from [`EventMetadata`]; the full event JSON
    /// goes into `payload_json` so [`Self::list`] can round-trip
    /// the typed value back.
    pub async fn append(&self, event: &T) -> super::Result<()> {
        let kind = event.kind().to_string();
        let agent_id = event.agent_id().to_string();
        let tenant_id = event.tenant_id().map(str::to_string);
        let at_ms = event.at_ms();
        let payload = serde_json::to_string(event)?;
        let sql = format!(
            "INSERT INTO {} (kind, agent_id, tenant_id, at_ms, payload_json)
                VALUES (?1, ?2, ?3, ?4, ?5)",
            self.table
        );
        sqlx::query(&sql)
            .bind(kind)
            .bind(agent_id)
            .bind(tenant_id)
            .bind(at_ms as i64)
            .bind(payload)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Run a filtered backfill query. Rows are returned newest
    /// first (`ORDER BY at_ms DESC, id DESC`). Malformed JSON rows
    /// — should never happen, but defended anyway — warn-log + are
    /// skipped silently rather than failing the whole query.
    pub async fn list(&self, filter: &ListFilter) -> super::Result<Vec<T>> {
        let mut limit = filter.limit;
        if limit == 0 {
            limit = DEFAULT_LIST_LIMIT;
        }
        limit = limit.min(MAX_LIST_LIMIT);

        let mut sql = format!("SELECT payload_json FROM {} WHERE 1=1", self.table);
        let mut idx = 1;
        if filter.agent_id.is_some() {
            sql.push_str(&format!(" AND agent_id = ?{idx}"));
            idx += 1;
        }
        if filter.kind.is_some() {
            sql.push_str(&format!(" AND kind = ?{idx}"));
            idx += 1;
        }
        if filter.tenant_id.is_some() {
            sql.push_str(&format!(" AND tenant_id = ?{idx}"));
            idx += 1;
        }
        if filter.since_ms.is_some() {
            sql.push_str(&format!(" AND at_ms >= ?{idx}"));
            idx += 1;
        }
        sql.push_str(&format!(" ORDER BY at_ms DESC, id DESC LIMIT ?{idx}"));

        let mut q = sqlx::query_as::<_, (String,)>(&sql);
        if let Some(a) = filter.agent_id.as_deref() {
            q = q.bind(a.to_string());
        }
        if let Some(k) = filter.kind.as_deref() {
            q = q.bind(k.to_string());
        }
        if let Some(t) = filter.tenant_id.as_deref() {
            q = q.bind(t.to_string());
        }
        if let Some(s) = filter.since_ms {
            q = q.bind(s as i64);
        }
        q = q.bind(limit as i64);

        let rows = q.fetch_all(&self.pool).await?;
        let mut out: Vec<T> = Vec::with_capacity(rows.len());
        for (json,) in rows {
            match serde_json::from_str::<T>(&json) {
                Ok(e) => out.push(e),
                Err(err) => tracing::warn!(
                    error = %err,
                    table = %self.table,
                    "EventStore::list skipped malformed row",
                ),
            }
        }
        Ok(out)
    }

    /// Two-phase retention sweep. Returns rows deleted across both
    /// phases.
    ///
    /// Phase 1 (time): drop everything with
    /// `at_ms < now - retention_days * 86_400 * 1000`.
    /// Phase 2 (cap): if `total > max_rows` after phase 1, drop
    /// `total - max_rows` oldest rows (`at_ms ASC, id ASC`).
    ///
    /// `(0, 0)` is the operator escape hatch — no-op so "keep
    /// everything forever" is reachable without conditional
    /// branching at the call site.
    pub async fn sweep_retention(
        &self,
        retention_days: u64,
        max_rows: usize,
    ) -> super::Result<usize> {
        if retention_days == 0 && max_rows == 0 {
            return Ok(0);
        }
        let mut deleted = 0usize;

        // Phase 1: time-based.
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let cutoff_ms = now_ms.saturating_sub(retention_days * 86_400 * 1000);
        let res = sqlx::query(&format!("DELETE FROM {} WHERE at_ms < ?", self.table))
            .bind(cutoff_ms as i64)
            .execute(&self.pool)
            .await?;
        deleted += res.rows_affected() as usize;

        // Phase 2: cap-based (oldest first).
        let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", self.table))
            .fetch_one(&self.pool)
            .await?;
        if (total as usize) > max_rows {
            let excess = (total as usize) - max_rows;
            let res = sqlx::query(&format!(
                "DELETE FROM {table} WHERE id IN (
                    SELECT id FROM {table}
                    ORDER BY at_ms ASC, id ASC LIMIT ?
                )",
                table = self.table
            ))
            .bind(excess as i64)
            .execute(&self.pool)
            .await?;
            deleted += res.rows_affected() as usize;
        }
        Ok(deleted)
    }
}

/// Reject anything that isn't `[A-Za-z_][A-Za-z0-9_]*`. Table
/// names are configured at boot, not user-supplied at runtime, so
/// this is defence-in-depth — but cheap.
fn validate_table_ident(table: &str) -> super::Result<()> {
    let mut chars = table.chars();
    let first = chars
        .next()
        .ok_or_else(|| super::EventsError::InvalidTable("must not be empty".into()))?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(super::EventsError::InvalidTable(format!(
            "must start with a letter or underscore, got {table:?}"
        )));
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return Err(super::EventsError::InvalidTable(format!(
                "must be [A-Za-z_][A-Za-z0-9_]*, got {table:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::agent_events::{AgentEventKind, TranscriptRole};
    use nexo_tool_meta::admin::escalations::{EscalationReason, EscalationUrgency};
    use nexo_tool_meta::admin::processing::{ProcessingControlState, ProcessingScope};
    use uuid::Uuid;

    fn convo(agent: &str) -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: agent.into(),
            channel: "whatsapp".into(),
            account_id: "55-1234".into(),
            contact_id: "55-5678".into(),
            mcp_channel_source: None,
        }
    }

    fn transcript(agent: &str, sent_at_ms: u64, tenant: Option<&str>) -> AgentEventKind {
        AgentEventKind::TranscriptAppended {
            agent_id: agent.into(),
            session_id: Uuid::nil(),
            seq: 0,
            role: TranscriptRole::User,
            body: "hola".into(),
            sent_at_ms,
            sender_id: None,
            source_plugin: "whatsapp".into(),
            tenant_id: tenant.map(String::from),
        }
    }

    fn pause(agent: &str, at_ms: u64) -> AgentEventKind {
        AgentEventKind::ProcessingStateChanged {
            agent_id: agent.into(),
            scope: convo(agent),
            prev_state: ProcessingControlState::AgentActive,
            new_state: ProcessingControlState::PausedByOperator {
                scope: convo(agent),
                paused_at_ms: at_ms,
                operator_token_hash: "h".into(),
                reason: None,
            },
            at_ms,
            tenant_id: None,
        }
    }

    fn esc(agent: &str, at_ms: u64) -> AgentEventKind {
        AgentEventKind::EscalationRequested {
            agent_id: agent.into(),
            scope: convo(agent),
            summary: "x".into(),
            reason: EscalationReason::UnknownQuery,
            urgency: EscalationUrgency::Normal,
            requested_at_ms: at_ms,
            tenant_id: None,
        }
    }

    async fn open() -> EventStore<AgentEventKind> {
        EventStore::open_memory(DEFAULT_TABLE).await.unwrap()
    }

    #[tokio::test]
    async fn append_then_list_round_trips() {
        let s = open().await;
        s.append(&transcript("ana", 1_000, None)).await.unwrap();
        s.append(&pause("ana", 2_000)).await.unwrap();
        let out = s
            .list(&ListFilter {
                agent_id: Some("ana".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        assert!(matches!(
            out[0],
            AgentEventKind::ProcessingStateChanged { .. }
        ));
    }

    #[tokio::test]
    async fn list_filters_by_agent_id() {
        let s = open().await;
        s.append(&transcript("ana", 1, None)).await.unwrap();
        s.append(&transcript("bob", 2, None)).await.unwrap();
        let out = s
            .list(&ListFilter {
                agent_id: Some("ana".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn list_filters_by_kind() {
        let s = open().await;
        s.append(&transcript("ana", 1, None)).await.unwrap();
        s.append(&pause("ana", 2)).await.unwrap();
        s.append(&esc("ana", 3)).await.unwrap();
        let out = s
            .list(&ListFilter {
                kind: Some("processing_state_changed".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0],
            AgentEventKind::ProcessingStateChanged { .. }
        ));
    }

    #[tokio::test]
    async fn list_filters_by_tenant_id() {
        let s = open().await;
        s.append(&transcript("ana", 1, Some("acme"))).await.unwrap();
        s.append(&transcript("ana", 2, Some("globex")))
            .await
            .unwrap();
        let out = s
            .list(&ListFilter {
                tenant_id: Some("acme".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn list_caps_limit() {
        let s = open().await;
        for i in 0..10u64 {
            s.append(&pause("ana", 1_000 + i)).await.unwrap();
        }
        let out = s
            .list(&ListFilter {
                limit: 3,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 3);
    }

    async fn row_count(s: &EventStore<AgentEventKind>) -> i64 {
        sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {DEFAULT_TABLE}"))
            .fetch_one(s.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn sweep_retention_deletes_old_rows_by_age() {
        let s = open().await;
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let day_ms: u64 = 86_400 * 1000;
        s.append(&pause("ana", now_ms - 60 * day_ms)).await.unwrap();
        s.append(&pause("ana", now_ms - 45 * day_ms)).await.unwrap();
        s.append(&pause("ana", now_ms - 31 * day_ms)).await.unwrap();
        s.append(&pause("ana", now_ms - 5 * day_ms)).await.unwrap();
        s.append(&pause("ana", now_ms - day_ms)).await.unwrap();
        let deleted = s.sweep_retention(10, 1_000_000).await.unwrap();
        assert_eq!(deleted, 3);
        assert_eq!(row_count(&s).await, 2);
    }

    #[tokio::test]
    async fn sweep_retention_caps_max_rows_dropping_oldest_first() {
        let s = open().await;
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        for i in 0..10u64 {
            s.append(&pause("ana", now_ms - (10 - i) * 1000))
                .await
                .unwrap();
        }
        let deleted = s.sweep_retention(36500, 3).await.unwrap();
        assert_eq!(deleted, 7);
        assert_eq!(row_count(&s).await, 3);
    }

    #[tokio::test]
    async fn sweep_retention_short_circuits_on_zero_zero() {
        let s = open().await;
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        for i in 0..5u64 {
            s.append(&pause("ana", now_ms - i)).await.unwrap();
        }
        let deleted = s.sweep_retention(0, 0).await.unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(row_count(&s).await, 5);
    }

    #[test]
    fn validate_table_ident_accepts_safe_names() {
        assert!(validate_table_ident("firehose_events").is_ok());
        assert!(validate_table_ident("_internal").is_ok());
        assert!(validate_table_ident("Events123").is_ok());
    }

    #[test]
    fn validate_table_ident_rejects_unsafe_names() {
        assert!(validate_table_ident("").is_err());
        assert!(validate_table_ident("1leading").is_err());
        assert!(validate_table_ident("has space").is_err());
        assert!(validate_table_ident("drop;table").is_err());
        assert!(validate_table_ident("with-dash").is_err());
    }
}
