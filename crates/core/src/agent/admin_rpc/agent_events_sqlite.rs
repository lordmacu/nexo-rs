//! Phase 82.11.log — SQLite-backed durable agent event log.
//!
//! v0 firehose ([`crate::agent::agent_events::BroadcastAgentEventEmitter`])
//! is in-process broadcast — subscribers that miss a window
//! lose events forever. With multiple new kinds shipping
//! (`ProcessingStateChanged`, `EscalationRequested`,
//! `EscalationResolved`, …) operator dashboards need
//! historical backfill across daemon restarts so a UI launched
//! after a state transition still sees what happened.
//!
//! Composes with [`crate::agent::agent_events::TeeAgentEventEmitter`]:
//! boot wires `Tee([Broadcast, SqliteAgentEventLog])` so every
//! emit reaches both live subscribers AND the durable log
//! without changing emit-site signatures. Reads go through the
//! [`AgentEventLog::list_recent`] API — orthogonal to the
//! existing `TranscriptReader` (which serves chat-session
//! JSONL); the SQLite log captures every kind including
//! non-transcript events.
//!
//! Single table, denormalised columns for the common filter
//! axes (`agent_id`, `tenant_id`, `kind`, `at_ms`). Full
//! `AgentEventKind` round-trips as JSON in `payload_json` so
//! future kinds land non-breaking — readers that don't know a
//! kind variant skip the row at deserialise time, the SQL
//! schema never changes.
//!
//! Open pattern matches `audit_sqlite` / `processing_sqlite` /
//! `escalations_sqlite`: WAL, idempotent
//! `CREATE TABLE IF NOT EXISTS`, `open_memory()` for tests.

use std::path::Path;
use std::str::FromStr;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use nexo_tool_meta::admin::agent_events::AgentEventKind;

use crate::agent::agent_events::AgentEventEmitter;

/// Read-side filter for [`AgentEventLog::list_recent`]. Mirrors
/// the shape `agent_events/list` admin RPC produces but lives
/// here so readers don't need the admin RPC types.
#[derive(Debug, Clone, Default)]
pub struct AgentEventLogFilter {
    /// Required — events for this agent.
    pub agent_id: String,
    /// Optional discriminator filter (e.g. `"processing_state_changed"`).
    pub kind: Option<String>,
    /// Lower-bound timestamp (epoch ms). `None` means no bound.
    pub since_ms: Option<u64>,
    /// Optional tenant filter.
    pub tenant_id: Option<String>,
    /// Hard cap. `0` defaults to 50 to match
    /// `AgentEventsListFilter`.
    pub limit: usize,
}

/// Read+write surface for the durable log.
#[async_trait]
pub trait AgentEventLog: Send + Sync + std::fmt::Debug {
    /// Persist one event. Best-effort — failures log + drop so
    /// a broken durable sink never blocks the live firehose.
    async fn append(&self, event: AgentEventKind) -> anyhow::Result<()>;

    /// Most-recent events matching `filter`, newest first.
    async fn list_recent(
        &self,
        filter: &AgentEventLogFilter,
    ) -> anyhow::Result<Vec<AgentEventKind>>;
}

/// SQLite implementation. Cheap to clone — pool is `Arc`-shared.
#[derive(Debug, Clone)]
pub struct SqliteAgentEventLog {
    pool: SqlitePool,
}

const DEFAULT_LIST_LIMIT: usize = 50;
const MAX_LIST_LIMIT: usize = 500;

impl SqliteAgentEventLog {
    /// Open or create the DB at `path`. Idempotent — DDL runs
    /// on every boot.
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
        Ok(Self { pool })
    }

    /// In-memory variant for tests.
    pub async fn open_memory() -> anyhow::Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::run_ddl(&pool).await?;
        Ok(Self { pool })
    }

    async fn run_ddl(pool: &SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS nexo_agent_events (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                kind          TEXT NOT NULL,
                agent_id      TEXT NOT NULL,
                tenant_id     TEXT,
                at_ms         INTEGER NOT NULL,
                payload_json  TEXT NOT NULL
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nexo_agent_events_agent
                ON nexo_agent_events(agent_id, at_ms DESC)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nexo_agent_events_tenant
                ON nexo_agent_events(tenant_id, at_ms DESC)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nexo_agent_events_kind
                ON nexo_agent_events(kind, at_ms DESC)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Boot-time retention sweep. Deletes rows older than
    /// `retention_days` AND drops the oldest beyond `max_rows`.
    /// Returns total rows deleted. Errors propagate — caller
    /// (boot supervisor) logs them; the live firehose never
    /// depends on this fn.
    ///
    /// Same shape as
    /// [`crate::agent::admin_rpc::SqliteAdminAuditWriter::sweep_retention`]
    /// so boot can run them in lockstep with one shared
    /// scheduler.
    pub async fn sweep_retention(
        &self,
        retention_days: u64,
        max_rows: usize,
    ) -> anyhow::Result<usize> {
        let mut deleted = 0usize;

        // 1. Time-based delete.
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let cutoff_ms = now_ms.saturating_sub(retention_days * 86_400 * 1000);
        let res = sqlx::query("DELETE FROM nexo_agent_events WHERE at_ms < ?")
            .bind(cutoff_ms as i64)
            .execute(&self.pool)
            .await?;
        deleted += res.rows_affected() as usize;

        // 2. Cap-based delete (oldest first).
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nexo_agent_events")
            .fetch_one(&self.pool)
            .await?;
        if (total as usize) > max_rows {
            let excess = (total as usize) - max_rows;
            let res = sqlx::query(
                "DELETE FROM nexo_agent_events WHERE id IN (
                    SELECT id FROM nexo_agent_events
                    ORDER BY at_ms ASC, id ASC LIMIT ?
                )",
            )
            .bind(excess as i64)
            .execute(&self.pool)
            .await?;
            deleted += res.rows_affected() as usize;
        }
        Ok(deleted)
    }
}

/// Pull the denormalised columns out of a typed event. Keeps
/// the SQL schema stable across enum additions — every row has
/// the same shape regardless of which variant it carries.
fn extract_metadata(event: &AgentEventKind) -> Option<EventMetadata> {
    match event {
        AgentEventKind::TranscriptAppended {
            agent_id,
            sent_at_ms,
            tenant_id,
            ..
        } => Some(EventMetadata {
            kind: "transcript_appended",
            agent_id: agent_id.clone(),
            tenant_id: tenant_id.clone(),
            at_ms: *sent_at_ms,
        }),
        AgentEventKind::PendingInboundsDropped {
            agent_id,
            at_ms,
            ..
        } => Some(EventMetadata {
            kind: "pending_inbounds_dropped",
            agent_id: agent_id.clone(),
            tenant_id: None,
            at_ms: *at_ms,
        }),
        AgentEventKind::EscalationRequested {
            agent_id,
            requested_at_ms,
            tenant_id,
            ..
        } => Some(EventMetadata {
            kind: "escalation_requested",
            agent_id: agent_id.clone(),
            tenant_id: tenant_id.clone(),
            at_ms: *requested_at_ms,
        }),
        AgentEventKind::EscalationResolved {
            agent_id,
            resolved_at_ms,
            tenant_id,
            ..
        } => Some(EventMetadata {
            kind: "escalation_resolved",
            agent_id: agent_id.clone(),
            tenant_id: tenant_id.clone(),
            at_ms: *resolved_at_ms,
        }),
        AgentEventKind::ProcessingStateChanged {
            agent_id,
            at_ms,
            tenant_id,
            ..
        } => Some(EventMetadata {
            kind: "processing_state_changed",
            agent_id: agent_id.clone(),
            tenant_id: tenant_id.clone(),
            at_ms: *at_ms,
        }),
        // `#[non_exhaustive]` — unknown variants from a future
        // tool-meta version skip the durable log instead of
        // panicking. Live subscribers still see them via
        // Broadcast.
        _ => None,
    }
}

struct EventMetadata {
    kind: &'static str,
    agent_id: String,
    tenant_id: Option<String>,
    at_ms: u64,
}

#[async_trait]
impl AgentEventLog for SqliteAgentEventLog {
    async fn append(&self, event: AgentEventKind) -> anyhow::Result<()> {
        let Some(meta) = extract_metadata(&event) else {
            tracing::warn!(
                "agent_events_sqlite: skipping unknown event variant"
            );
            return Ok(());
        };
        let payload = serde_json::to_string(&event)?;
        sqlx::query(
            "INSERT INTO nexo_agent_events
                (kind, agent_id, tenant_id, at_ms, payload_json)
                VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(meta.kind)
        .bind(&meta.agent_id)
        .bind(&meta.tenant_id)
        .bind(meta.at_ms as i64)
        .bind(&payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_recent(
        &self,
        filter: &AgentEventLogFilter,
    ) -> anyhow::Result<Vec<AgentEventKind>> {
        let mut limit = filter.limit;
        if limit == 0 {
            limit = DEFAULT_LIST_LIMIT;
        }
        limit = limit.min(MAX_LIST_LIMIT);

        // Build query dynamically so the SQL planner picks the
        // best index. Defense-in-depth: we never interpolate
        // user-controlled strings — bindings stay parameterised.
        let mut sql = String::from(
            "SELECT payload_json FROM nexo_agent_events WHERE agent_id = ?1",
        );
        let mut bind_index = 2;
        if filter.kind.is_some() {
            sql.push_str(&format!(" AND kind = ?{bind_index}"));
            bind_index += 1;
        }
        if filter.tenant_id.is_some() {
            sql.push_str(&format!(" AND tenant_id = ?{bind_index}"));
            bind_index += 1;
        }
        if filter.since_ms.is_some() {
            sql.push_str(&format!(" AND at_ms >= ?{bind_index}"));
            bind_index += 1;
        }
        sql.push_str(&format!(" ORDER BY at_ms DESC, id DESC LIMIT ?{bind_index}"));

        let mut q = sqlx::query_as::<_, (String,)>(&sql).bind(&filter.agent_id);
        if let Some(k) = filter.kind.as_deref() {
            q = q.bind(k);
        }
        if let Some(t) = filter.tenant_id.as_deref() {
            q = q.bind(t);
        }
        if let Some(s) = filter.since_ms {
            q = q.bind(s as i64);
        }
        q = q.bind(limit as i64);

        let rows = q.fetch_all(&self.pool).await?;
        let mut out: Vec<AgentEventKind> = Vec::with_capacity(rows.len());
        for (json,) in rows {
            match serde_json::from_str::<AgentEventKind>(&json) {
                Ok(e) => out.push(e),
                Err(err) => tracing::warn!(
                    error = %err,
                    "agent_events_sqlite: list skipped malformed row",
                ),
            }
        }
        Ok(out)
    }
}

/// Convenience — `SqliteAgentEventLog` doubles as an
/// `AgentEventEmitter` so boot wires it directly into Tee
/// alongside Broadcast. Failures inside `append` log + drop
/// rather than propagate (firehose is best-effort).
#[async_trait]
impl AgentEventEmitter for SqliteAgentEventLog {
    async fn emit(&self, event: AgentEventKind) {
        if let Err(e) = self.append(event).await {
            tracing::warn!(
                error = %e,
                "agent_events_sqlite: append failed; live firehose continues",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::agent_events::TranscriptRole;
    use nexo_tool_meta::admin::escalations::{EscalationReason, EscalationUrgency};
    use nexo_tool_meta::admin::processing::{
        ProcessingControlState, ProcessingScope,
    };
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

    fn transcript(agent: &str, seq: u64, sent_at_ms: u64) -> AgentEventKind {
        AgentEventKind::TranscriptAppended {
            agent_id: agent.into(),
            session_id: Uuid::nil(),
            seq,
            role: TranscriptRole::User,
            body: "hola".into(),
            sent_at_ms,
            sender_id: None,
            source_plugin: "whatsapp".into(),
            tenant_id: None,
        }
    }

    fn pause_changed(
        agent: &str,
        at_ms: u64,
        tenant_id: Option<String>,
    ) -> AgentEventKind {
        let scope = convo(agent);
        AgentEventKind::ProcessingStateChanged {
            agent_id: agent.into(),
            scope: scope.clone(),
            prev_state: ProcessingControlState::AgentActive,
            new_state: ProcessingControlState::PausedByOperator {
                scope,
                paused_at_ms: at_ms,
                operator_token_hash: "abcdef0123456789".into(),
                reason: None,
            },
            at_ms,
            tenant_id,
        }
    }

    fn escalation(agent: &str, requested_at_ms: u64) -> AgentEventKind {
        AgentEventKind::EscalationRequested {
            agent_id: agent.into(),
            scope: convo(agent),
            summary: "needs review".into(),
            reason: EscalationReason::UnknownQuery,
            urgency: EscalationUrgency::Normal,
            requested_at_ms,
            tenant_id: None,
        }
    }

    #[tokio::test]
    async fn append_then_list_round_trips_typed_event() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        log.append(transcript("ana", 0, 1_000)).await.unwrap();
        log.append(pause_changed("ana", 2_000, None)).await.unwrap();
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        // Newest first → ProcessingStateChanged at 2_000 leads
        // TranscriptAppended at 1_000.
        match &out[0] {
            AgentEventKind::ProcessingStateChanged { at_ms, .. } => {
                assert_eq!(*at_ms, 2_000)
            }
            other => panic!("unexpected first event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_filters_by_agent_id() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        log.append(transcript("ana", 0, 1_000)).await.unwrap();
        log.append(transcript("bob", 0, 1_500)).await.unwrap();
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn list_filters_by_kind() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        log.append(transcript("ana", 0, 1_000)).await.unwrap();
        log.append(pause_changed("ana", 2_000, None)).await.unwrap();
        log.append(escalation("ana", 3_000)).await.unwrap();
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
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
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        log.append(pause_changed("ana", 1_000, Some("acme".into())))
            .await
            .unwrap();
        log.append(pause_changed("ana", 2_000, Some("globex".into())))
            .await
            .unwrap();
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                tenant_id: Some("acme".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentEventKind::ProcessingStateChanged { tenant_id, .. } => {
                assert_eq!(tenant_id.as_deref(), Some("acme"))
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_filters_by_since_ms() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        log.append(pause_changed("ana", 1_000, None)).await.unwrap();
        log.append(pause_changed("ana", 2_000, None)).await.unwrap();
        log.append(pause_changed("ana", 3_000, None)).await.unwrap();
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                since_ms: Some(2_000),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
    }

    #[tokio::test]
    async fn list_caps_limit() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        for i in 0..10u64 {
            log.append(pause_changed("ana", 1_000 + i, None))
                .await
                .unwrap();
        }
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                limit: 3,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 3);
    }

    #[tokio::test]
    async fn list_default_limit_when_zero() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        for i in 0..60u64 {
            log.append(pause_changed("ana", 1_000 + i, None))
                .await
                .unwrap();
        }
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                limit: 0, // → DEFAULT_LIST_LIMIT (50)
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 50);
    }

    #[tokio::test]
    async fn emit_routes_to_append() {
        // SqliteAgentEventLog as AgentEventEmitter (Tee sink).
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        let log_dyn: &dyn AgentEventEmitter = &log;
        log_dyn.emit(pause_changed("ana", 1_000, None)).await;
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn list_returns_empty_for_unknown_agent() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        log.append(transcript("ana", 0, 1_000)).await.unwrap();
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "nonexistent".into(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn append_persists_across_handle_clone() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        log.append(pause_changed("ana", 1_000, None)).await.unwrap();
        // Clone handle (shares pool) — second handle sees the
        // row written through the first.
        let log2 = log.clone();
        let out = log2
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn sweep_retention_deletes_old_rows_by_age() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let day_ms = 86_400_000u64;
        // 100d ago: should be deleted under 60d retention.
        log.append(pause_changed("ana", now_ms - 100 * day_ms, None))
            .await
            .unwrap();
        // 30d ago: should survive.
        log.append(pause_changed("ana", now_ms - 30 * day_ms, None))
            .await
            .unwrap();
        let deleted = log.sweep_retention(60, 10_000).await.unwrap();
        assert_eq!(deleted, 1);
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 1, "30d-old row survives");
    }

    #[tokio::test]
    async fn sweep_retention_caps_max_rows_dropping_oldest_first() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        // 5 rows, all recent (timestamps anchored to "now" so the
        // age-based delete leaves them all alone — only the
        // cap-based delete fires).
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        for i in 0..5u64 {
            log.append(pause_changed("ana", now_ms + i, None))
                .await
                .unwrap();
        }
        // Cap to 2 → 3 oldest dropped.
        let deleted = log.sweep_retention(365, 2).await.unwrap();
        assert_eq!(deleted, 3);
        let out = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        // Survivors are the two newest (i=3 and i=4).
        match (&out[0], &out[1]) {
            (
                AgentEventKind::ProcessingStateChanged { at_ms: a, .. },
                AgentEventKind::ProcessingStateChanged { at_ms: b, .. },
            ) => {
                assert_eq!(*a, now_ms + 4);
                assert_eq!(*b, now_ms + 3);
            }
            other => panic!("unexpected survivors: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sweep_retention_is_idempotent_when_nothing_to_drop() {
        let log = SqliteAgentEventLog::open_memory().await.unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        log.append(pause_changed("ana", now_ms, None)).await.unwrap();
        let first = log.sweep_retention(90, 100).await.unwrap();
        let second = log.sweep_retention(90, 100).await.unwrap();
        assert_eq!(first, 0);
        assert_eq!(second, 0);
    }
}
