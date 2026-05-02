//! Phase 82.14.c — SQLite-backed `EscalationStore`.
//!
//! Durable variant of the in-memory escalation store so pending
//! human-review requests survive daemon restarts. Without
//! durability the operator UI shows "0 pending" right after a
//! bounce even when the agent had flagged scopes for review —
//! the customer's request silently waits forever.
//!
//! Single-table design keyed by canonical scope JSON, mirroring
//! the in-memory store's `DashMap<ProcessingScope, EscalationEntry>`
//! shape. The full `EscalationEntry` round-trips as JSON so
//! future state variants (e.g. `Snoozed { until }`) land
//! non-breaking. `agent_id` is denormalised onto its own column
//! so the future durable `list` filter (`agent_id = ?`) hits an
//! index path rather than parsing every row.
//!
//! Mirrors the audit_sqlite + processing_sqlite open pattern:
//! WAL, idempotent `CREATE TABLE IF NOT EXISTS`, `open_memory()`
//! variant for tests.

use std::path::Path;
use std::str::FromStr;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use nexo_tool_meta::admin::escalations::{
    EscalationEntry, EscalationState, EscalationsListParams, ResolvedBy,
};
use nexo_tool_meta::admin::processing::ProcessingScope;

use super::domains::escalations::{filter_matches, EscalationStore};

/// SQLite-backed `EscalationStore`. Cheap to clone — the pool is
/// `Arc`-shared internally.
#[derive(Debug, Clone)]
pub struct SqliteEscalationStore {
    pool: SqlitePool,
}

impl SqliteEscalationStore {
    /// Open or create the DB at `path`. Idempotent — DDL runs on
    /// every boot.
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
            "CREATE TABLE IF NOT EXISTS nexo_escalations (
                scope_json   TEXT PRIMARY KEY,
                agent_id     TEXT NOT NULL,
                entry_json   TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await?;
        // Per-agent index so future server-side filtering scales
        // when the operator hosts hundreds of agents.
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nexo_escalations_agent
                ON nexo_escalations(agent_id, updated_at_ms DESC)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }
}

fn scope_key(scope: &ProcessingScope) -> anyhow::Result<String> {
    let v = serde_json::to_value(scope)?;
    Ok(canonicalize(&v).to_string())
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize(v)))
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => {
            let v: Vec<Value> = arr.iter().map(canonicalize).collect();
            Value::Array(v)
        }
        other => other.clone(),
    }
}

#[async_trait]
impl EscalationStore for SqliteEscalationStore {
    async fn list(
        &self,
        filter: &EscalationsListParams,
    ) -> anyhow::Result<Vec<EscalationEntry>> {
        // Load every row, deserialise, then run the existing
        // `filter_matches` predicate so the SQL stays simple +
        // the trait contract (filter / agent / scope_kind) lives
        // in one place. Server-side push-down lands in 82.14.c.b
        // alongside the per-tenant filter wire-up.
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT entry_json FROM nexo_escalations")
                .fetch_all(&self.pool)
                .await?;
        let mut out: Vec<EscalationEntry> = Vec::with_capacity(rows.len());
        for (json,) in rows {
            match serde_json::from_str::<EscalationEntry>(&json) {
                Ok(e) => {
                    if filter_matches(&e, filter) {
                        out.push(e);
                    }
                }
                Err(err) => tracing::warn!(
                    error = %err,
                    "escalations_sqlite: list skipped malformed row",
                ),
            }
        }
        // Newest first by request/resolve time. Same ordering as
        // InMemoryEscalationStore.
        out.sort_by_key(|e| match &e.state {
            EscalationState::Pending { requested_at_ms, .. } => {
                std::cmp::Reverse(*requested_at_ms)
            }
            EscalationState::Resolved { resolved_at_ms, .. } => {
                std::cmp::Reverse(*resolved_at_ms)
            }
            _ => std::cmp::Reverse(0u64),
        });
        out.truncate(filter.limit);
        Ok(out)
    }

    async fn get(&self, scope: &ProcessingScope) -> anyhow::Result<EscalationState> {
        let key = scope_key(scope)?;
        let row: Option<(String,)> =
            sqlx::query_as("SELECT entry_json FROM nexo_escalations WHERE scope_json = ?1")
                .bind(&key)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            None => Ok(EscalationState::None),
            Some((json,)) => {
                let entry: EscalationEntry = serde_json::from_str(&json)?;
                Ok(entry.state)
            }
        }
    }

    async fn resolve(
        &self,
        scope: &ProcessingScope,
        by: ResolvedBy,
        resolved_at_ms: u64,
    ) -> anyhow::Result<bool> {
        let key = scope_key(scope)?;
        let row: Option<(String,)> =
            sqlx::query_as("SELECT entry_json FROM nexo_escalations WHERE scope_json = ?1")
                .bind(&key)
                .fetch_optional(&self.pool)
                .await?;
        let Some((json,)) = row else {
            return Ok(false);
        };
        let mut entry: EscalationEntry = serde_json::from_str(&json)?;
        if !matches!(entry.state, EscalationState::Pending { .. }) {
            return Ok(false);
        }
        entry.state = EscalationState::Resolved {
            scope: scope.clone(),
            resolved_at_ms,
            by,
        };
        let payload = serde_json::to_string(&entry)?;
        sqlx::query(
            "UPDATE nexo_escalations \
             SET entry_json = ?2, updated_at_ms = ?3 \
             WHERE scope_json = ?1",
        )
        .bind(&key)
        .bind(&payload)
        .bind(resolved_at_ms as i64)
        .execute(&self.pool)
        .await?;
        Ok(true)
    }

    async fn upsert_pending(
        &self,
        agent_id: String,
        state: EscalationState,
    ) -> anyhow::Result<bool> {
        let scope = match &state {
            EscalationState::Pending { scope, .. }
            | EscalationState::Resolved { scope, .. } => scope.clone(),
            _ => anyhow::bail!("upsert_pending requires Pending or Resolved state"),
        };
        let key = scope_key(&scope)?;
        let prev: Option<(String,)> =
            sqlx::query_as("SELECT entry_json FROM nexo_escalations WHERE scope_json = ?1")
                .bind(&key)
                .fetch_optional(&self.pool)
                .await?;
        let entry = EscalationEntry {
            agent_id: agent_id.clone(),
            scope: scope.clone(),
            state,
        };
        let payload = serde_json::to_string(&entry)?;
        let updated_at_ms = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO nexo_escalations (scope_json, agent_id, entry_json, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(scope_json) DO UPDATE SET \
                 agent_id = excluded.agent_id, \
                 entry_json = excluded.entry_json, \
                 updated_at_ms = excluded.updated_at_ms",
        )
        .bind(&key)
        .bind(&agent_id)
        .bind(&payload)
        .bind(updated_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(prev.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::escalations::{
        EscalationReason, EscalationUrgency, EscalationsListFilter,
    };
    use std::collections::BTreeMap;

    fn convo(agent: &str, peer: &str) -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: agent.into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: peer.into(),
            mcp_channel_source: None,
        }
    }

    fn pending_state(scope: ProcessingScope, ts: u64) -> EscalationState {
        EscalationState::Pending {
            scope,
            summary: "needs human".into(),
            reason: EscalationReason::OutOfScope,
            urgency: EscalationUrgency::High,
            context: BTreeMap::new(),
            requested_at_ms: ts,
        }
    }

    fn list_params(limit: usize) -> EscalationsListParams {
        EscalationsListParams {
            filter: EscalationsListFilter::default(),
            agent_id: None,
            scope_kind: None,
            limit,
            tenant_id: None,
        }
    }

    #[tokio::test]
    async fn missing_scope_returns_none_state() {
        let store = SqliteEscalationStore::open_memory().await.unwrap();
        let state = store.get(&convo("ana", "wa.1")).await.unwrap();
        assert!(matches!(state, EscalationState::None));
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips_through_sqlite() {
        let store = SqliteEscalationStore::open_memory().await.unwrap();
        let scope = convo("ana", "wa.1");
        let inserted = store
            .upsert_pending("ana".into(), pending_state(scope.clone(), 1_000))
            .await
            .unwrap();
        assert!(inserted, "first upsert reports new row");
        let state = store.get(&scope).await.unwrap();
        match state {
            EscalationState::Pending { requested_at_ms, .. } => {
                assert_eq!(requested_at_ms, 1_000);
            }
            other => panic!("expected Pending, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upsert_idempotent_returns_false_on_repeat() {
        let store = SqliteEscalationStore::open_memory().await.unwrap();
        let scope = convo("ana", "wa.1");
        assert!(store
            .upsert_pending("ana".into(), pending_state(scope.clone(), 1))
            .await
            .unwrap());
        assert!(!store
            .upsert_pending("ana".into(), pending_state(scope, 2))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn resolve_flips_pending_to_resolved() {
        let store = SqliteEscalationStore::open_memory().await.unwrap();
        let scope = convo("ana", "wa.1");
        store
            .upsert_pending("ana".into(), pending_state(scope.clone(), 1))
            .await
            .unwrap();
        let changed = store
            .resolve(&scope, ResolvedBy::OperatorTakeover, 2_000)
            .await
            .unwrap();
        assert!(changed);
        match store.get(&scope).await.unwrap() {
            EscalationState::Resolved { resolved_at_ms, by, .. } => {
                assert_eq!(resolved_at_ms, 2_000);
                assert!(matches!(by, ResolvedBy::OperatorTakeover));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_returns_false_when_already_resolved() {
        let store = SqliteEscalationStore::open_memory().await.unwrap();
        let scope = convo("ana", "wa.1");
        store
            .upsert_pending("ana".into(), pending_state(scope.clone(), 1))
            .await
            .unwrap();
        store
            .resolve(&scope, ResolvedBy::OperatorTakeover, 2)
            .await
            .unwrap();
        let changed = store
            .resolve(&scope, ResolvedBy::OperatorTakeover, 3)
            .await
            .unwrap();
        assert!(!changed, "resolving an already-Resolved row is a no-op");
    }

    #[tokio::test]
    async fn resolve_returns_false_for_unknown_scope() {
        let store = SqliteEscalationStore::open_memory().await.unwrap();
        let changed = store
            .resolve(&convo("ana", "wa.999"), ResolvedBy::OperatorTakeover, 1)
            .await
            .unwrap();
        assert!(!changed);
    }

    #[tokio::test]
    async fn list_returns_pending_newest_first_and_truncates() {
        let store = SqliteEscalationStore::open_memory().await.unwrap();
        for (peer, ts) in [("wa.1", 1_000u64), ("wa.2", 3_000), ("wa.3", 2_000)] {
            store
                .upsert_pending("ana".into(), pending_state(convo("ana", peer), ts))
                .await
                .unwrap();
        }
        let rows = store.list(&list_params(50)).await.unwrap();
        assert_eq!(rows.len(), 3);
        let timestamps: Vec<u64> = rows
            .into_iter()
            .map(|e| match e.state {
                EscalationState::Pending { requested_at_ms, .. } => requested_at_ms,
                _ => 0,
            })
            .collect();
        assert_eq!(timestamps, vec![3_000, 2_000, 1_000]);
        // limit truncates.
        let two = store.list(&list_params(2)).await.unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(
            match &two[0].state {
                EscalationState::Pending { requested_at_ms, .. } => *requested_at_ms,
                _ => 0,
            },
            3_000
        );
    }

    #[tokio::test]
    async fn list_filters_by_agent_id() {
        let store = SqliteEscalationStore::open_memory().await.unwrap();
        store
            .upsert_pending("ana".into(), pending_state(convo("ana", "wa.1"), 1))
            .await
            .unwrap();
        store
            .upsert_pending("bob".into(), pending_state(convo("bob", "wa.2"), 2))
            .await
            .unwrap();
        let mut p = list_params(50);
        p.agent_id = Some("ana".into());
        let rows = store.list(&p).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent_id, "ana");
    }

    #[tokio::test]
    async fn state_survives_pool_round_trip_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escalations.db");
        let store = SqliteEscalationStore::open(&path).await.unwrap();
        let scope = convo("ana", "wa.1");
        store
            .upsert_pending("ana".into(), pending_state(scope.clone(), 99))
            .await
            .unwrap();
        drop(store);
        let store2 = SqliteEscalationStore::open(&path).await.unwrap();
        match store2.get(&scope).await.unwrap() {
            EscalationState::Pending { requested_at_ms, .. } => {
                assert_eq!(requested_at_ms, 99, "Pending must survive restart");
            }
            other => panic!("expected Pending, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ddl_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escalations.db");
        let _w1 = SqliteEscalationStore::open(&path).await.unwrap();
        let _w2 = SqliteEscalationStore::open(&path).await.unwrap();
    }
}
