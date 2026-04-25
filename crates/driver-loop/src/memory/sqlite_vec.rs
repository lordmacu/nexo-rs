//! `SqliteVecDecisionMemory` — sqlite-vec backed `DecisionMemory`.
//!
//! Schema lives in `driver_decisions` + `driver_decisions_vec` tables
//! and is created on first `open` (idempotent migration).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use nexo_driver_claude::ClaudeError;
use nexo_driver_permission::PermissionRequest;
use nexo_driver_types::{Decision, DecisionChoice, DecisionId, GoalId};
use nexo_memory::{vector, EmbeddingProvider};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::memory::prompt::{decision_to_text, request_to_text};
use crate::memory::trait_def::{DecisionMemory, Namespace};

const SCHEMA_VERSION: i64 = 1;

pub struct SqliteVecDecisionMemory {
    pool: SqlitePool,
    embedder: Arc<dyn EmbeddingProvider>,
    namespace: Namespace,
    dim: usize,
}

impl SqliteVecDecisionMemory {
    pub async fn open(
        path: &str,
        embedder: Arc<dyn EmbeddingProvider>,
    ) -> Result<Self, ClaudeError> {
        // Register sqlite-vec as an auto-extension. Idempotent.
        vector::enable();

        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let max_conns = if path == ":memory:" { 1 } else { 4 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conns)
            .connect_with(opts)
            .await
            .map_err(|e| ClaudeError::Binding(e.to_string()))?;

        if path != ":memory:" {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&pool)
                .await
                .map_err(|e| ClaudeError::Binding(e.to_string()))?;
            sqlx::query("PRAGMA synchronous = NORMAL")
                .execute(&pool)
                .await
                .map_err(|e| ClaudeError::Binding(e.to_string()))?;
        }

        let dim = embedder.dimension();
        Self::migrate(&pool, dim).await?;

        Ok(Self {
            pool,
            embedder,
            namespace: Namespace::Global,
            dim,
        })
    }

    pub async fn open_memory(embedder: Arc<dyn EmbeddingProvider>) -> Result<Self, ClaudeError> {
        Self::open(":memory:", embedder).await
    }

    pub fn with_namespace(mut self, ns: Namespace) -> Self {
        self.namespace = ns;
        self
    }

    /// Test helper.
    #[doc(hidden)]
    pub fn pool_for_test(&self) -> &SqlitePool {
        &self.pool
    }

    /// Test helper.
    #[doc(hidden)]
    pub async fn count(&self) -> Result<u64, ClaudeError> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM driver_decisions")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| ClaudeError::Binding(e.to_string()))?;
        Ok(n as u64)
    }

    async fn migrate(pool: &SqlitePool, dim: usize) -> Result<(), ClaudeError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS driver_decisions (\
                id              TEXT PRIMARY KEY,\
                goal_id         TEXT NOT NULL,\
                turn_index      INTEGER NOT NULL,\
                tool            TEXT NOT NULL,\
                input_summary   TEXT NOT NULL,\
                choice_kind     TEXT NOT NULL,\
                choice_message  TEXT,\
                rationale       TEXT NOT NULL,\
                decided_at      INTEGER NOT NULL,\
                full_input_json TEXT NOT NULL,\
                schema_version  INTEGER NOT NULL DEFAULT 1\
            )",
        )
        .execute(pool)
        .await
        .map_err(|e| ClaudeError::Binding(e.to_string()))?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_dd_goal_id ON driver_decisions(goal_id)")
            .execute(pool)
            .await
            .map_err(|e| ClaudeError::Binding(e.to_string()))?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_dd_decided_at ON driver_decisions(decided_at)")
            .execute(pool)
            .await
            .map_err(|e| ClaudeError::Binding(e.to_string()))?;

        // dim mismatch detection — if vec table exists, compare via a sample row.
        let exists: Option<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND name='driver_decisions_vec'",
        )
        .fetch_optional(pool)
        .await
        .map_err(|e| ClaudeError::Binding(e.to_string()))?;

        if exists.is_none() {
            let sql = format!(
                "CREATE VIRTUAL TABLE driver_decisions_vec USING vec0(embedding FLOAT[{dim}])"
            );
            sqlx::query(&sql)
                .execute(pool)
                .await
                .map_err(|e| ClaudeError::Binding(e.to_string()))?;
        } else {
            let sample: Option<(Vec<u8>,)> =
                sqlx::query_as("SELECT embedding FROM driver_decisions_vec LIMIT 1")
                    .fetch_optional(pool)
                    .await
                    .ok()
                    .flatten();
            if let Some((bytes,)) = sample {
                let existing_dim = bytes.len() / 4;
                if existing_dim != dim {
                    return Err(ClaudeError::Binding(format!(
                        "decision-memory dim mismatch: schema={existing_dim}, embedder={dim}; \
                         drop the table or reset the file"
                    )));
                }
            }
        }

        sqlx::query(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
            .execute(pool)
            .await
            .map_err(|e| ClaudeError::Binding(e.to_string()))?;
        Ok(())
    }
}

fn choice_kind_label(c: &DecisionChoice) -> (&'static str, Option<String>) {
    match c {
        DecisionChoice::Allow => ("allow", None),
        DecisionChoice::Deny { message } => ("deny", Some(message.clone())),
        DecisionChoice::Observe { note } => ("observe", Some(note.clone())),
    }
}

fn parse_choice(kind: &str, message: Option<String>) -> DecisionChoice {
    match kind {
        "allow" => DecisionChoice::Allow,
        "deny" => DecisionChoice::Deny {
            message: message.unwrap_or_default(),
        },
        "observe" => DecisionChoice::Observe {
            note: message.unwrap_or_default(),
        },
        _ => DecisionChoice::Allow,
    }
}

#[async_trait]
impl DecisionMemory for SqliteVecDecisionMemory {
    async fn record(&self, decision: &Decision) -> Result<(), ClaudeError> {
        let text = decision_to_text(decision);
        let mut vecs = match self.embedder.embed(&[text.as_str()]).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "decision-memory", "embed record failed: {e}");
                return Ok(());
            }
        };
        if vecs.is_empty() {
            return Ok(());
        }
        let v = vecs.remove(0);
        if v.len() != self.dim {
            tracing::warn!(
                target: "decision-memory",
                "embed dim mismatch: got {}, expected {}",
                v.len(),
                self.dim
            );
            return Ok(());
        }
        let bytes = vector::pack_f32(&v);

        let (choice_kind, choice_message) = choice_kind_label(&decision.choice);
        let full_input_json =
            serde_json::to_string(&decision.input).unwrap_or_else(|_| "null".into());

        // Transactional insert — use a manual rowid linkage by reading
        // last_insert_rowid() in the same transaction.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ClaudeError::Binding(e.to_string()))?;

        let inserted = sqlx::query(
            "INSERT INTO driver_decisions (\
                id, goal_id, turn_index, tool, input_summary, \
                choice_kind, choice_message, rationale, decided_at, full_input_json\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(decision.id.0.to_string())
        .bind(decision.goal_id.0.to_string())
        .bind(decision.turn_index as i64)
        .bind(&decision.tool)
        .bind(&text)
        .bind(choice_kind)
        .bind(choice_message)
        .bind(&decision.rationale)
        .bind(decision.decided_at.timestamp())
        .bind(&full_input_json)
        .execute(&mut *tx)
        .await
        .map_err(|e| ClaudeError::Binding(e.to_string()))?;

        if inserted.rows_affected() == 0 {
            // Duplicate id — keep existing embedding row untouched.
            tx.commit()
                .await
                .map_err(|e| ClaudeError::Binding(e.to_string()))?;
            return Ok(());
        }

        let rowid: (i64,) = sqlx::query_as("SELECT rowid FROM driver_decisions WHERE id = ?")
            .bind(decision.id.0.to_string())
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ClaudeError::Binding(e.to_string()))?;

        sqlx::query("INSERT INTO driver_decisions_vec(rowid, embedding) VALUES (?, ?)")
            .bind(rowid.0)
            .bind(bytes)
            .execute(&mut *tx)
            .await
            .map_err(|e| ClaudeError::Binding(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| ClaudeError::Binding(e.to_string()))?;
        Ok(())
    }

    async fn recall(&self, req: &PermissionRequest, k: usize) -> Vec<Decision> {
        if k == 0 {
            return Vec::new();
        }
        let text = request_to_text(req);
        let mut vecs = match self.embedder.embed(&[text.as_str()]).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "decision-memory", "embed recall failed: {e}");
                return Vec::new();
            }
        };
        if vecs.is_empty() {
            return Vec::new();
        }
        let v = vecs.remove(0);
        if v.len() != self.dim {
            return Vec::new();
        }
        let bytes = vector::pack_f32(&v);

        let goal_filter: Option<String> = match &self.namespace {
            Namespace::PerGoal(g) => Some(g.0.to_string()),
            Namespace::Global => None,
        };

        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                i64,
                String,
                String,
                Option<String>,
                String,
                i64,
                String,
            ),
        >(
            "SELECT d.id, d.goal_id, d.turn_index, d.tool, \
                    d.choice_kind, d.choice_message, d.rationale, \
                    d.decided_at, d.full_input_json \
             FROM driver_decisions_vec v \
             JOIN driver_decisions d ON d.rowid = v.rowid \
             WHERE v.embedding MATCH ?1 \
               AND v.k = ?2 \
               AND (?3 IS NULL OR d.goal_id = ?3) \
             ORDER BY v.distance",
        )
        .bind(bytes)
        .bind(k as i64)
        .bind(goal_filter)
        .fetch_all(&self.pool)
        .await;

        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(target: "decision-memory", "recall query failed: {e}");
                return Vec::new();
            }
        };

        let mut out = Vec::with_capacity(rows.len());
        for (
            id,
            goal_id,
            turn_index,
            tool,
            choice_kind,
            choice_msg,
            rationale,
            decided_at,
            input_json,
        ) in rows
        {
            let id = match Uuid::parse_str(&id) {
                Ok(u) => DecisionId(u),
                Err(_) => continue,
            };
            let goal_id = match Uuid::parse_str(&goal_id) {
                Ok(u) => GoalId(u),
                Err(_) => continue,
            };
            let input: serde_json::Value =
                serde_json::from_str(&input_json).unwrap_or(serde_json::Value::Null);
            let decided_at = Utc
                .timestamp_opt(decided_at, 0)
                .single()
                .unwrap_or_else(Utc::now);
            out.push(Decision {
                id,
                goal_id,
                turn_index: turn_index as u32,
                tool,
                input,
                choice: parse_choice(&choice_kind, choice_msg),
                rationale,
                decided_at,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::mock::MockEmbedder;
    use chrono::Utc;
    use nexo_driver_types::DecisionId;
    use serde_json::json;

    fn dec(tool: &str, input: serde_json::Value) -> Decision {
        Decision {
            id: DecisionId::new(),
            goal_id: GoalId::new(),
            turn_index: 0,
            tool: tool.into(),
            input,
            choice: DecisionChoice::Allow,
            rationale: "ok".into(),
            decided_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn open_creates_schema_and_count_zero() {
        let m = SqliteVecDecisionMemory::open_memory(Arc::new(MockEmbedder::new()))
            .await
            .unwrap();
        assert_eq!(m.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn record_persists_and_count_increments() {
        let m = SqliteVecDecisionMemory::open_memory(Arc::new(MockEmbedder::new()))
            .await
            .unwrap();
        m.record(&dec("Edit", json!({"file": "x"}))).await.unwrap();
        m.record(&dec("Bash", json!({"cmd": "ls"}))).await.unwrap();
        assert_eq!(m.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn recall_returns_at_most_k() {
        let m = SqliteVecDecisionMemory::open_memory(Arc::new(MockEmbedder::new()))
            .await
            .unwrap();
        for i in 0..5 {
            m.record(&dec("Edit", json!({"file": format!("f{i}.rs")})))
                .await
                .unwrap();
        }
        let req = PermissionRequest {
            goal_id: GoalId::new(),
            tool_use_id: "tu".into(),
            tool_name: "Edit".into(),
            input: json!({"file": "f0.rs"}),
            metadata: serde_json::Map::new(),
        };
        let hits = m.recall(&req, 3).await;
        assert!(hits.len() <= 3);
        assert!(!hits.is_empty(), "expected at least one hit");
    }

    #[tokio::test]
    async fn record_idempotent_on_duplicate_id() {
        let m = SqliteVecDecisionMemory::open_memory(Arc::new(MockEmbedder::new()))
            .await
            .unwrap();
        let d = dec("Edit", json!({"a": 1}));
        m.record(&d).await.unwrap();
        m.record(&d).await.unwrap();
        assert_eq!(m.count().await.unwrap(), 1);
    }
}
