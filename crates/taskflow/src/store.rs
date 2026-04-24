use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use uuid::Uuid;

use crate::types::{Flow, FlowError, FlowEvent, FlowStatus, FlowStep, FlowStepStatus, StepRuntime};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS flows (
    id TEXT PRIMARY KEY,
    controller_id TEXT NOT NULL,
    goal TEXT NOT NULL,
    owner_session_key TEXT NOT NULL,
    requester_origin TEXT NOT NULL,
    current_step TEXT NOT NULL,
    state_json TEXT NOT NULL DEFAULT '{}',
    wait_json TEXT,
    status TEXT NOT NULL,
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_flows_owner ON flows(owner_session_key);
CREATE INDEX IF NOT EXISTS idx_flows_status ON flows(status);

CREATE TABLE IF NOT EXISTS flow_steps (
    id TEXT PRIMARY KEY,
    flow_id TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
    runtime TEXT NOT NULL,
    child_session_key TEXT,
    run_id TEXT NOT NULL,
    task TEXT NOT NULL,
    status TEXT NOT NULL,
    result_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_flow_steps_flow ON flow_steps(flow_id);

-- Phase-14 follow-up: the engine looks up steps by `(flow_id, run_id)`
-- on every observation event. Without the unique index, two
-- concurrent observations could both see "no row" in
-- `find_step_by_run_id` and each insert a fresh row — duplicate steps
-- with the same run_id, followed by non-deterministic lookups.
-- UNIQUE also fixes the perf issue (was O(n) per observation).
CREATE UNIQUE INDEX IF NOT EXISTS idx_flow_steps_run
    ON flow_steps(flow_id, run_id);

CREATE TABLE IF NOT EXISTS flow_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_flow_events_flow ON flow_events(flow_id);
"#;

/// Persistence layer for `Flow` records.
///
/// All mutating operations are revision-checked. Stale callers receive
/// `FlowError::RevisionMismatch` and must re-fetch before retrying.
#[async_trait]
pub trait FlowStore: Send + Sync {
    async fn insert(&self, flow: &Flow) -> Result<(), FlowError>;
    async fn get(&self, id: Uuid) -> Result<Option<Flow>, FlowError>;
    async fn list_by_owner(&self, owner_session_key: &str) -> Result<Vec<Flow>, FlowError>;
    async fn list_by_status(&self, status: FlowStatus) -> Result<Vec<Flow>, FlowError>;
    async fn update_with_revision(&self, flow: &Flow) -> Result<Flow, FlowError>;
    async fn append_event(
        &self,
        flow_id: Uuid,
        kind: &str,
        payload: Value,
    ) -> Result<FlowEvent, FlowError>;
    async fn list_events(&self, flow_id: Uuid, limit: i64) -> Result<Vec<FlowEvent>, FlowError>;

    // ---- flow_steps ----
    async fn insert_step(&self, step: &FlowStep) -> Result<(), FlowError>;
    async fn update_step(&self, step: &FlowStep) -> Result<FlowStep, FlowError>;
    async fn get_step(&self, id: Uuid) -> Result<Option<FlowStep>, FlowError>;
    async fn list_steps(&self, flow_id: Uuid) -> Result<Vec<FlowStep>, FlowError>;
    async fn find_step_by_run_id(
        &self,
        flow_id: Uuid,
        run_id: &str,
    ) -> Result<Option<FlowStep>, FlowError>;

    /// Update a flow's revision + append an audit event atomically.
    /// Previously `FlowManager::with_retry` called `update_with_revision`
    /// followed by `append_event` as two round-trips — a crash between
    /// them left the flow updated but with no event row, silently
    /// corrupting the audit trail. Implementers should run both in a
    /// single transaction; the default impl here is safe for stores
    /// where atomic multi-op isn't possible (falls back to the
    /// non-atomic pair with a warn log).
    async fn update_and_append(
        &self,
        flow: &Flow,
        event_kind: &str,
        event_payload: Value,
    ) -> Result<(Flow, FlowEvent), FlowError> {
        tracing::warn!(
            flow_id = %flow.id,
            "FlowStore::update_and_append using non-atomic fallback — implement a transaction-based override"
        );
        let updated = self.update_with_revision(flow).await?;
        let event = self
            .append_event(updated.id, event_kind, event_payload)
            .await?;
        Ok((updated, event))
    }

    /// Drop flows in terminal status (`Finished`, `Failed`, `Cancelled`)
    /// whose `updated_at` is older than `retain_days`. Cascades through
    /// `flow_steps` and `flow_events` via ON DELETE CASCADE in schema.
    /// Intended for a daily heartbeat so `list_by_owner` / `list_by_
    /// status` don't grow O(n) over all history. Default impl returns
    /// an error — stores must override.
    async fn prune_terminal_flows(&self, _retain_days: i64) -> Result<u64, FlowError> {
        Err(FlowError::InvalidData(
            "prune_terminal_flows not implemented by this store".into(),
        ))
    }
}

#[derive(Clone)]
pub struct SqliteFlowStore {
    pool: SqlitePool,
}

impl SqliteFlowStore {
    /// Open or create a SQLite database at `path`. Use `:memory:` for tests.
    pub async fn open(path: &str) -> Result<Self, FlowError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))
            .map_err(|e| FlowError::InvalidData(format!("bad sqlite url: {e}")))?
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        Self::with_pool(pool).await
    }

    /// Open against a pool the caller already owns. Useful for tests that
    /// share an in-memory DB across multiple stores.
    pub async fn with_pool(pool: SqlitePool) -> Result<Self, FlowError> {
        // Run schema once. SQL is idempotent (`IF NOT EXISTS`).
        for stmt in SCHEMA.split(';') {
            let trimmed = stmt.trim();
            if trimmed.is_empty() {
                continue;
            }
            sqlx::query(trimmed).execute(&pool).await?;
        }
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl FlowStore for SqliteFlowStore {
    async fn insert(&self, flow: &Flow) -> Result<(), FlowError> {
        let state_json = serde_json::to_string(&flow.state_json)
            .map_err(|e| FlowError::InvalidData(e.to_string()))?;
        let wait_json = match &flow.wait_json {
            Some(v) => Some(
                serde_json::to_string(v).map_err(|e| FlowError::InvalidData(e.to_string()))?,
            ),
            None => None,
        };
        sqlx::query(
            "INSERT INTO flows (id, controller_id, goal, owner_session_key, requester_origin, \
             current_step, state_json, wait_json, status, cancel_requested, revision, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(flow.id.to_string())
        .bind(&flow.controller_id)
        .bind(&flow.goal)
        .bind(&flow.owner_session_key)
        .bind(&flow.requester_origin)
        .bind(&flow.current_step)
        .bind(state_json)
        .bind(wait_json)
        .bind(flow.status.as_str())
        .bind(flow.cancel_requested as i64)
        .bind(flow.revision)
        .bind(flow.created_at.to_rfc3339())
        .bind(flow.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: Uuid) -> Result<Option<Flow>, FlowError> {
        let row = sqlx::query("SELECT * FROM flows WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_flow).transpose()
    }

    async fn list_by_owner(&self, owner_session_key: &str) -> Result<Vec<Flow>, FlowError> {
        let rows = sqlx::query(
            "SELECT * FROM flows WHERE owner_session_key = ? ORDER BY created_at DESC",
        )
        .bind(owner_session_key)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_flow).collect()
    }

    async fn list_by_status(&self, status: FlowStatus) -> Result<Vec<Flow>, FlowError> {
        let rows = sqlx::query("SELECT * FROM flows WHERE status = ? ORDER BY updated_at ASC")
            .bind(status.as_str())
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(row_to_flow).collect()
    }

    async fn update_with_revision(&self, flow: &Flow) -> Result<Flow, FlowError> {
        let state_json = serde_json::to_string(&flow.state_json)
            .map_err(|e| FlowError::InvalidData(e.to_string()))?;
        let wait_json = match &flow.wait_json {
            Some(v) => Some(
                serde_json::to_string(v).map_err(|e| FlowError::InvalidData(e.to_string()))?,
            ),
            None => None,
        };
        let new_revision = flow.revision + 1;
        let now = Utc::now();
        let result = sqlx::query(
            "UPDATE flows SET controller_id = ?, goal = ?, owner_session_key = ?, \
             requester_origin = ?, current_step = ?, state_json = ?, wait_json = ?, \
             status = ?, cancel_requested = ?, revision = ?, updated_at = ? \
             WHERE id = ? AND revision = ?",
        )
        .bind(&flow.controller_id)
        .bind(&flow.goal)
        .bind(&flow.owner_session_key)
        .bind(&flow.requester_origin)
        .bind(&flow.current_step)
        .bind(state_json)
        .bind(wait_json)
        .bind(flow.status.as_str())
        .bind(flow.cancel_requested as i64)
        .bind(new_revision)
        .bind(now.to_rfc3339())
        .bind(flow.id.to_string())
        .bind(flow.revision)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            // Either gone, or stale revision. Disambiguate.
            let actual = self.get(flow.id).await?;
            return match actual {
                None => Err(FlowError::NotFound(flow.id)),
                Some(found) => Err(FlowError::RevisionMismatch {
                    expected: flow.revision,
                    actual: found.revision,
                }),
            };
        }
        // Refetch to return the canonical post-update state.
        self.get(flow.id)
            .await?
            .ok_or(FlowError::NotFound(flow.id))
    }

    async fn append_event(
        &self,
        flow_id: Uuid,
        kind: &str,
        payload: Value,
    ) -> Result<FlowEvent, FlowError> {
        let payload_json =
            serde_json::to_string(&payload).map_err(|e| FlowError::InvalidData(e.to_string()))?;
        let now = Utc::now();
        let result = sqlx::query(
            "INSERT INTO flow_events (flow_id, kind, payload_json, at) VALUES (?, ?, ?, ?)",
        )
        .bind(flow_id.to_string())
        .bind(kind)
        .bind(payload_json)
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(FlowEvent {
            id: result.last_insert_rowid(),
            flow_id,
            kind: kind.to_string(),
            payload_json: payload,
            at: now,
        })
    }

    async fn list_events(&self, flow_id: Uuid, limit: i64) -> Result<Vec<FlowEvent>, FlowError> {
        // Guard against negative limit — SQLite treats `LIMIT -1` as
        // unbounded which would scan the entire flow_events table for
        // a long-lived flow. Callers that want "all" should say so
        // explicitly via `i64::MAX`.
        let limit = limit.max(0);
        let rows = sqlx::query(
            "SELECT id, flow_id, kind, payload_json, at FROM flow_events \
             WHERE flow_id = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(flow_id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let id: i64 = row.try_get("id")?;
                let flow_id_s: String = row.try_get("flow_id")?;
                let kind: String = row.try_get("kind")?;
                let payload_s: String = row.try_get("payload_json")?;
                let at_s: String = row.try_get("at")?;
                let flow_id = Uuid::parse_str(&flow_id_s)
                    .map_err(|e| FlowError::InvalidData(format!("bad flow_id: {e}")))?;
                let payload_json = serde_json::from_str(&payload_s)
                    .map_err(|e| FlowError::InvalidData(format!("bad event payload: {e}")))?;
                let at = chrono::DateTime::parse_from_rfc3339(&at_s)
                    .map_err(|e| FlowError::InvalidData(format!("bad event ts: {e}")))?
                    .with_timezone(&Utc);
                Ok(FlowEvent {
                    id,
                    flow_id,
                    kind,
                    payload_json,
                    at,
                })
            })
            .collect()
    }

    async fn insert_step(&self, step: &FlowStep) -> Result<(), FlowError> {
        let result_s = match &step.result_json {
            Some(v) => Some(
                serde_json::to_string(v).map_err(|e| FlowError::InvalidData(e.to_string()))?,
            ),
            None => None,
        };
        sqlx::query(
            "INSERT INTO flow_steps (id, flow_id, runtime, child_session_key, run_id, task, status, result_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(step.id.to_string())
        .bind(step.flow_id.to_string())
        .bind(step.runtime.as_str())
        .bind(step.child_session_key.as_deref())
        .bind(&step.run_id)
        .bind(&step.task)
        .bind(step.status.as_str())
        .bind(result_s)
        .bind(step.created_at.to_rfc3339())
        .bind(step.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_step(&self, step: &FlowStep) -> Result<FlowStep, FlowError> {
        let result_s = match &step.result_json {
            Some(v) => Some(
                serde_json::to_string(v).map_err(|e| FlowError::InvalidData(e.to_string()))?,
            ),
            None => None,
        };
        let now = Utc::now();
        let rows = sqlx::query(
            "UPDATE flow_steps SET runtime = ?, child_session_key = ?, run_id = ?, task = ?, \
             status = ?, result_json = ?, updated_at = ? WHERE id = ?",
        )
        .bind(step.runtime.as_str())
        .bind(step.child_session_key.as_deref())
        .bind(&step.run_id)
        .bind(&step.task)
        .bind(step.status.as_str())
        .bind(result_s)
        .bind(now.to_rfc3339())
        .bind(step.id.to_string())
        .execute(&self.pool)
        .await?;
        if rows.rows_affected() == 0 {
            return Err(FlowError::NotFound(step.id));
        }
        self.get_step(step.id)
            .await?
            .ok_or(FlowError::NotFound(step.id))
    }

    async fn get_step(&self, id: Uuid) -> Result<Option<FlowStep>, FlowError> {
        let row = sqlx::query("SELECT * FROM flow_steps WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_step).transpose()
    }

    async fn list_steps(&self, flow_id: Uuid) -> Result<Vec<FlowStep>, FlowError> {
        let rows = sqlx::query(
            "SELECT * FROM flow_steps WHERE flow_id = ? ORDER BY created_at ASC",
        )
        .bind(flow_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_step).collect()
    }

    async fn find_step_by_run_id(
        &self,
        flow_id: Uuid,
        run_id: &str,
    ) -> Result<Option<FlowStep>, FlowError> {
        let row = sqlx::query("SELECT * FROM flow_steps WHERE flow_id = ? AND run_id = ?")
            .bind(flow_id.to_string())
            .bind(run_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_step).transpose()
    }

    async fn prune_terminal_flows(&self, retain_days: i64) -> Result<u64, FlowError> {
        // Cutoff as RFC3339 so the comparison matches the stored
        // `updated_at` text format (SQLite string compare works for
        // ISO8601 UTC strings because the encoding is sort-preserving).
        let cutoff = (Utc::now() - chrono::Duration::days(retain_days.max(0))).to_rfc3339();
        let result = sqlx::query(
            "DELETE FROM flows \
             WHERE status IN ('finished','failed','cancelled') \
               AND updated_at < ?",
        )
        .bind(&cutoff)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Atomic override: runs the revision check, UPDATE, event INSERT,
    /// and final SELECT inside one SQLite transaction. A crash at any
    /// point rolls back and the caller sees either the full old state
    /// or the full new state + event — no torn audit trail.
    async fn update_and_append(
        &self,
        flow: &Flow,
        event_kind: &str,
        event_payload: Value,
    ) -> Result<(Flow, FlowEvent), FlowError> {
        let state_json = serde_json::to_string(&flow.state_json)
            .map_err(|e| FlowError::InvalidData(e.to_string()))?;
        let wait_json = match &flow.wait_json {
            Some(v) => Some(
                serde_json::to_string(v).map_err(|e| FlowError::InvalidData(e.to_string()))?,
            ),
            None => None,
        };
        let payload_json = serde_json::to_string(&event_payload)
            .map_err(|e| FlowError::InvalidData(e.to_string()))?;
        let new_revision = flow.revision + 1;
        let now = Utc::now();

        let mut tx = self.pool.begin().await?;
        let update_result = sqlx::query(
            "UPDATE flows SET controller_id = ?, goal = ?, owner_session_key = ?, \
             requester_origin = ?, current_step = ?, state_json = ?, wait_json = ?, \
             status = ?, cancel_requested = ?, revision = ?, updated_at = ? \
             WHERE id = ? AND revision = ?",
        )
        .bind(&flow.controller_id)
        .bind(&flow.goal)
        .bind(&flow.owner_session_key)
        .bind(&flow.requester_origin)
        .bind(&flow.current_step)
        .bind(state_json)
        .bind(wait_json)
        .bind(flow.status.as_str())
        .bind(flow.cancel_requested as i64)
        .bind(new_revision)
        .bind(now.to_rfc3339())
        .bind(flow.id.to_string())
        .bind(flow.revision)
        .execute(&mut *tx)
        .await?;

        if update_result.rows_affected() == 0 {
            // Roll back — the caller re-fetches via update_with_revision
            // path to distinguish NotFound vs RevisionMismatch.
            tx.rollback().await?;
            let actual = self.get(flow.id).await?;
            return match actual {
                None => Err(FlowError::NotFound(flow.id)),
                Some(found) => Err(FlowError::RevisionMismatch {
                    expected: flow.revision,
                    actual: found.revision,
                }),
            };
        }

        let event_result = sqlx::query(
            "INSERT INTO flow_events (flow_id, kind, payload_json, at) VALUES (?, ?, ?, ?)",
        )
        .bind(flow.id.to_string())
        .bind(event_kind)
        .bind(payload_json)
        .bind(now.to_rfc3339())
        .execute(&mut *tx)
        .await?;
        let event_id = event_result.last_insert_rowid();

        // Read the canonical post-commit flow state inside the tx.
        let row = sqlx::query("SELECT * FROM flows WHERE id = ?")
            .bind(flow.id.to_string())
            .fetch_one(&mut *tx)
            .await?;
        let updated = row_to_flow(row)?;

        tx.commit().await?;

        let event = FlowEvent {
            id: event_id,
            flow_id: flow.id,
            kind: event_kind.to_string(),
            payload_json: event_payload,
            at: now,
        };
        Ok((updated, event))
    }
}

fn row_to_step(row: sqlx::sqlite::SqliteRow) -> Result<FlowStep, FlowError> {
    let id_s: String = row.try_get("id")?;
    let id = Uuid::parse_str(&id_s).map_err(|e| FlowError::InvalidData(format!("bad id: {e}")))?;
    let flow_id_s: String = row.try_get("flow_id")?;
    let flow_id = Uuid::parse_str(&flow_id_s)
        .map_err(|e| FlowError::InvalidData(format!("bad flow_id: {e}")))?;
    let runtime_s: String = row.try_get("runtime")?;
    let runtime = StepRuntime::from_str(&runtime_s)
        .ok_or_else(|| FlowError::InvalidData(format!("unknown runtime: {runtime_s}")))?;
    let child_session_key: Option<String> = row.try_get("child_session_key")?;
    let run_id: String = row.try_get("run_id")?;
    let task: String = row.try_get("task")?;
    let status_s: String = row.try_get("status")?;
    let status = FlowStepStatus::from_str(&status_s)
        .ok_or_else(|| FlowError::InvalidData(format!("unknown step status: {status_s}")))?;
    let result_s: Option<String> = row.try_get("result_json")?;
    let result_json = match result_s {
        Some(s) => Some(
            serde_json::from_str::<Value>(&s)
                .map_err(|e| FlowError::InvalidData(format!("bad result_json: {e}")))?,
        ),
        None => None,
    };
    let created_at_s: String = row.try_get("created_at")?;
    let updated_at_s: String = row.try_get("updated_at")?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_s)
        .map_err(|e| FlowError::InvalidData(format!("bad created_at: {e}")))?
        .with_timezone(&Utc);
    let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_s)
        .map_err(|e| FlowError::InvalidData(format!("bad updated_at: {e}")))?
        .with_timezone(&Utc);

    Ok(FlowStep {
        id,
        flow_id,
        runtime,
        child_session_key,
        run_id,
        task,
        status,
        result_json,
        created_at,
        updated_at,
    })
}

fn row_to_flow(row: sqlx::sqlite::SqliteRow) -> Result<Flow, FlowError> {
    let id_s: String = row.try_get("id")?;
    let id = Uuid::parse_str(&id_s).map_err(|e| FlowError::InvalidData(format!("bad id: {e}")))?;
    let controller_id: String = row.try_get("controller_id")?;
    let goal: String = row.try_get("goal")?;
    let owner_session_key: String = row.try_get("owner_session_key")?;
    let requester_origin: String = row.try_get("requester_origin")?;
    let current_step: String = row.try_get("current_step")?;
    let state_json_s: String = row.try_get("state_json")?;
    let wait_json_s: Option<String> = row.try_get("wait_json")?;
    let status_s: String = row.try_get("status")?;
    let cancel_requested_i: i64 = row.try_get("cancel_requested")?;
    let revision: i64 = row.try_get("revision")?;
    let created_at_s: String = row.try_get("created_at")?;
    let updated_at_s: String = row.try_get("updated_at")?;

    let state_json: Value = serde_json::from_str(&state_json_s)
        .map_err(|e| FlowError::InvalidData(format!("bad state_json: {e}")))?;
    let wait_json = match wait_json_s {
        Some(s) => Some(
            serde_json::from_str::<Value>(&s)
                .map_err(|e| FlowError::InvalidData(format!("bad wait_json: {e}")))?,
        ),
        None => None,
    };
    let status = FlowStatus::from_str(&status_s)
        .ok_or_else(|| FlowError::InvalidData(format!("unknown status: {status_s}")))?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_s)
        .map_err(|e| FlowError::InvalidData(format!("bad created_at: {e}")))?
        .with_timezone(&Utc);
    let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_s)
        .map_err(|e| FlowError::InvalidData(format!("bad updated_at: {e}")))?
        .with_timezone(&Utc);

    Ok(Flow {
        id,
        controller_id,
        goal,
        owner_session_key,
        requester_origin,
        current_step,
        state_json,
        wait_json,
        status,
        cancel_requested: cancel_requested_i != 0,
        revision,
        created_at,
        updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_flow() -> Flow {
        let now = Utc::now();
        Flow {
            id: Uuid::new_v4(),
            controller_id: "kate/inbox-triage".into(),
            goal: "triage inbox".into(),
            owner_session_key: "agent:kate:session:abc".into(),
            requester_origin: "user-1".into(),
            current_step: "classify".into(),
            state_json: json!({"messages": 10, "processed": 0}),
            wait_json: None,
            status: FlowStatus::Created,
            cancel_requested: false,
            revision: 0,
            created_at: now,
            updated_at: now,
        }
    }

    async fn store() -> SqliteFlowStore {
        SqliteFlowStore::open(":memory:").await.expect("open")
    }

    #[tokio::test]
    async fn insert_then_get_round_trip() {
        let s = store().await;
        let flow = sample_flow();
        s.insert(&flow).await.expect("insert");
        let got = s.get(flow.id).await.expect("get").expect("found");
        assert_eq!(got.id, flow.id);
        assert_eq!(got.controller_id, "kate/inbox-triage");
        assert_eq!(got.state_json, flow.state_json);
        assert_eq!(got.status, FlowStatus::Created);
        assert_eq!(got.revision, 0);
        assert!(!got.cancel_requested);
    }

    #[tokio::test]
    async fn list_by_owner_returns_only_matching() {
        let s = store().await;
        let mut a = sample_flow();
        a.owner_session_key = "owner-A".into();
        let mut b = sample_flow();
        b.owner_session_key = "owner-B".into();
        let mut a2 = sample_flow();
        a2.owner_session_key = "owner-A".into();
        s.insert(&a).await.unwrap();
        s.insert(&b).await.unwrap();
        s.insert(&a2).await.unwrap();
        let owned = s.list_by_owner("owner-A").await.unwrap();
        assert_eq!(owned.len(), 2);
        assert!(owned.iter().all(|f| f.owner_session_key == "owner-A"));
    }

    #[tokio::test]
    async fn update_with_correct_revision_succeeds_and_bumps() {
        let s = store().await;
        let flow = sample_flow();
        s.insert(&flow).await.unwrap();

        let mut updated = flow.clone();
        updated.status = FlowStatus::Running;
        updated.current_step = "fetch".into();
        let result = s.update_with_revision(&updated).await.expect("update");
        assert_eq!(result.revision, 1);
        assert_eq!(result.status, FlowStatus::Running);
        assert_eq!(result.current_step, "fetch");
    }

    #[tokio::test]
    async fn update_with_stale_revision_returns_mismatch() {
        let s = store().await;
        let flow = sample_flow();
        s.insert(&flow).await.unwrap();

        // First update: 0 → 1
        let mut first = flow.clone();
        first.status = FlowStatus::Running;
        s.update_with_revision(&first).await.unwrap();

        // Second update with stale revision (still 0) should fail.
        let mut stale = flow.clone();
        stale.status = FlowStatus::Waiting;
        let err = s.update_with_revision(&stale).await.err().expect("err");
        match err {
            FlowError::RevisionMismatch { expected, actual } => {
                assert_eq!(expected, 0);
                assert_eq!(actual, 1);
            }
            other => panic!("expected RevisionMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_by_status_filters() {
        let s = store().await;
        let mut a = sample_flow();
        a.status = FlowStatus::Waiting;
        let mut b = sample_flow();
        b.status = FlowStatus::Running;
        s.insert(&a).await.unwrap();
        s.insert(&b).await.unwrap();
        let waiting = s.list_by_status(FlowStatus::Waiting).await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].id, a.id);
    }

    fn sample_step(flow_id: Uuid, run_id: &str) -> FlowStep {
        let now = Utc::now();
        FlowStep {
            id: Uuid::new_v4(),
            flow_id,
            runtime: StepRuntime::Managed,
            child_session_key: Some("child:session:1".into()),
            run_id: run_id.into(),
            task: "classify messages".into(),
            status: FlowStepStatus::Pending,
            result_json: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn insert_and_get_step() {
        let s = store().await;
        let flow = sample_flow();
        s.insert(&flow).await.unwrap();
        let step = sample_step(flow.id, "run-1");
        s.insert_step(&step).await.unwrap();
        let got = s.get_step(step.id).await.unwrap().expect("found");
        assert_eq!(got.flow_id, flow.id);
        assert_eq!(got.run_id, "run-1");
        assert_eq!(got.runtime, StepRuntime::Managed);
        assert_eq!(got.status, FlowStepStatus::Pending);
    }

    #[tokio::test]
    async fn update_step_changes_status_and_result() {
        let s = store().await;
        let flow = sample_flow();
        s.insert(&flow).await.unwrap();
        let mut step = sample_step(flow.id, "run-1");
        s.insert_step(&step).await.unwrap();
        step.status = FlowStepStatus::Succeeded;
        step.result_json = Some(json!({"count": 5}));
        let updated = s.update_step(&step).await.unwrap();
        assert_eq!(updated.status, FlowStepStatus::Succeeded);
        assert_eq!(updated.result_json.unwrap()["count"], 5);
    }

    #[tokio::test]
    async fn list_steps_filters_by_flow_id_and_orders_ascending() {
        let s = store().await;
        let a = sample_flow();
        let b = sample_flow();
        s.insert(&a).await.unwrap();
        s.insert(&b).await.unwrap();
        s.insert_step(&sample_step(a.id, "a-1")).await.unwrap();
        s.insert_step(&sample_step(a.id, "a-2")).await.unwrap();
        s.insert_step(&sample_step(b.id, "b-1")).await.unwrap();
        let a_steps = s.list_steps(a.id).await.unwrap();
        assert_eq!(a_steps.len(), 2);
        assert_eq!(a_steps[0].run_id, "a-1"); // insertion order, ASC
        assert_eq!(a_steps[1].run_id, "a-2");
        let b_steps = s.list_steps(b.id).await.unwrap();
        assert_eq!(b_steps.len(), 1);
        assert_eq!(b_steps[0].run_id, "b-1");
    }

    #[tokio::test]
    async fn find_step_by_run_id_scopes_to_flow() {
        let s = store().await;
        let a = sample_flow();
        s.insert(&a).await.unwrap();
        s.insert_step(&sample_step(a.id, "run-same")).await.unwrap();
        let hit = s
            .find_step_by_run_id(a.id, "run-same")
            .await
            .unwrap()
            .expect("found");
        assert_eq!(hit.flow_id, a.id);
        let miss = s.find_step_by_run_id(a.id, "run-other").await.unwrap();
        assert!(miss.is_none());
    }

    #[tokio::test]
    async fn update_unknown_step_returns_not_found() {
        let s = store().await;
        let flow = sample_flow();
        s.insert(&flow).await.unwrap();
        let step = sample_step(flow.id, "ghost");
        // Never inserted — update should fail.
        let err = s.update_step(&step).await.err().expect("err");
        assert!(matches!(err, FlowError::NotFound(_)));
    }

    #[tokio::test]
    async fn append_and_list_events() {
        let s = store().await;
        let flow = sample_flow();
        s.insert(&flow).await.unwrap();
        s.append_event(flow.id, "created", json!({"goal": flow.goal}))
            .await
            .unwrap();
        s.append_event(flow.id, "advanced", json!({"step": "fetch"}))
            .await
            .unwrap();
        let events = s.list_events(flow.id, 10).await.unwrap();
        assert_eq!(events.len(), 2);
        // ORDER BY id DESC — most recent first.
        assert_eq!(events[0].kind, "advanced");
        assert_eq!(events[1].kind, "created");
    }
}
