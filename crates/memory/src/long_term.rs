use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::concepts::{derive_concept_tags, MAX_CONCEPT_TAGS};
use crate::embedding::EmbeddingProvider;
use crate::relevance::MemoryType;
use crate::secret_scanner::SecretGuard;
use crate::vector;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: Uuid,
    pub agent_id: String,
    pub content: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub concept_tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// Phase 77.6 — memory type for per-type half-life decay in scoring.
    /// None = legacy record, treated as Project (most conservative default).
    #[serde(default)]
    pub memory_type: Option<MemoryType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredInteraction {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub role: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderEntry {
    pub id: Uuid,
    pub agent_id: String,
    pub session_id: Uuid,
    pub plugin: String,
    pub recipient: String,
    pub message: String,
    pub due_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmailFollowupStatus {
    Active,
    Cancelled,
    Completed,
    Exhausted,
}

impl EmailFollowupStatus {
    fn from_db(raw: &str) -> Self {
        match raw {
            "cancelled" => EmailFollowupStatus::Cancelled,
            "completed" => EmailFollowupStatus::Completed,
            "exhausted" => EmailFollowupStatus::Exhausted,
            _ => EmailFollowupStatus::Active,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailFollowupEntry {
    pub flow_id: Uuid,
    pub agent_id: String,
    pub session_id: Uuid,
    pub source_plugin: String,
    pub source_instance: Option<String>,
    pub recipient: String,
    pub thread_root_id: String,
    pub instruction: String,
    pub check_every_secs: u64,
    pub max_attempts: u32,
    pub attempts: u32,
    pub next_check_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub status: EmailFollowupStatus,
    pub status_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct EmailFollowupDbRow {
    flow_id: String,
    agent_id: String,
    session_id: String,
    source_plugin: String,
    source_instance: Option<String>,
    recipient: String,
    thread_root_id: String,
    instruction: String,
    check_every_secs: i64,
    max_attempts: i64,
    attempts: i64,
    next_check_at: i64,
    claimed_at: Option<i64>,
    status: String,
    status_note: Option<String>,
    created_at: i64,
    updated_at: i64,
}

pub struct LongTermMemory {
    pool: SqlitePool,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
    guard: Option<SecretGuard>,
    /// Phase 36.2 — optional mutation observer. Fires once per write
    /// (insert / update / delete) so the snapshot subsystem (or any
    /// other audit consumer) can stream every memory mutation onto
    /// the `nexo.memory.mutated.<agent_id>` NATS subject. Absent by
    /// default; boot wire injects a real impl when
    /// `memory.snapshot.events.mutation_publish_enabled = true`.
    mutation_hook: Option<Arc<dyn nexo_driver_types::MemoryMutationHook>>,
    /// Tenant string passed to the mutation hook. Defaults to
    /// `"default"` for single-tenant deployments; SaaS deployments
    /// wire the per-binding tenant via `with_tenant`.
    tenant: String,
}

impl LongTermMemory {
    pub async fn open(path: &str) -> anyhow::Result<Self> {
        Self::open_with_vector(path, None).await
    }

    /// Phase 5.4 — open with an optional embedding provider. When supplied,
    /// `sqlite-vec` is registered as an auto-extension and the `vec_memories`
    /// virtual table is created (dimension taken from the provider). A
    /// dimension mismatch against an existing DB aborts with a clear error
    /// pointing at the fix (delete the DB).
    pub async fn open_with_vector(
        path: &str,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        if embedding.is_some() {
            vector::enable();
        }

        let url = format!("sqlite://{}?mode=rwc", path);
        let pool = SqlitePool::connect(&url).await?;

        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA foreign_keys=ON").execute(&pool).await?;

        let store = Self {
            pool,
            embedding,
            guard: None,
            mutation_hook: None,
            tenant: "default".into(),
        };
        store.migrate().await?;
        if let Some(provider) = &store.embedding {
            store.init_vector_schema(provider.dimension()).await?;
        }
        Ok(store)
    }

    pub fn embedding_provider(&self) -> Option<&Arc<dyn EmbeddingProvider>> {
        self.embedding.as_ref()
    }

    /// Attach a secret guard. When set, `remember_typed()` scans content
    /// before every INSERT and applies the guard's policy (Block / Redact / Warn).
    pub fn with_guard(mut self, guard: SecretGuard) -> Self {
        self.guard = Some(guard);
        self
    }

    /// Phase 36.2 — attach a mutation observer. Fires best-effort on
    /// every successful `remember_typed` / `forget` write. Hook
    /// failure must never poison the writer's transaction (the trait
    /// contract enforces this — `on_mutation` returns `()`).
    pub fn with_mutation_hook(
        mut self,
        hook: Arc<dyn nexo_driver_types::MemoryMutationHook>,
    ) -> Self {
        self.mutation_hook = Some(hook);
        self
    }

    /// Override the tenant string passed to the mutation hook.
    /// Default is `"default"` so single-tenant deployments do not
    /// need to set anything.
    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = tenant.into();
        self
    }

    async fn init_vector_schema(&self, dim: usize) -> anyhow::Result<()> {
        let exists: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='vec_memories'",
        )
        .fetch_optional(&self.pool)
        .await?;

        if exists.is_none() {
            // Create using the declared dimension; sqlite-vec's `vec0`
            // module parses the schema from the CREATE TABLE statement.
            let sql = format!(
                "CREATE VIRTUAL TABLE vec_memories USING vec0(\
                    memory_id TEXT PRIMARY KEY,\
                    embedding FLOAT[{dim}]\
                )"
            );
            sqlx::query(&sql).execute(&self.pool).await?;
            return Ok(());
        }

        // Table exists — probe the current dimension via a no-op insert
        // with a zero-vector and compare errors. Cheaper alternative:
        // SELECT one row's embedding length if any exist. Use the latter.
        let sample: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT embedding FROM vec_memories LIMIT 1")
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        if let Some((bytes,)) = sample {
            let existing_dim = bytes.len() / 4;
            if existing_dim != dim {
                anyhow::bail!(
                    "vec_memories dimension mismatch: db={existing_dim}, config={dim}; \
                     delete the memory db file to rebuild the vector index"
                );
            }
        }
        // Empty table: we can't detect dim from rows, but sqlite-vec would
        // reject an INSERT with wrong width at that point. Acceptable.
        Ok(())
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS memories (
                id         TEXT PRIMARY KEY,
                agent_id   TEXT NOT NULL,
                content    TEXT NOT NULL,
                tags       TEXT NOT NULL DEFAULT '[]',
                created_at INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memories_agent
             ON memories(agent_id, created_at DESC)",
        )
        .execute(&self.pool)
        .await?;

        // concept_tags — auto-derived via `derive_concept_tags`, stored as JSON.
        // Idempotent: swallow the "duplicate column" error on re-runs but
        // surface anything else (disk full, permission denied, etc.) so a
        // real migration failure doesn't leave the schema half-applied.
        if let Err(e) =
            sqlx::query("ALTER TABLE memories ADD COLUMN concept_tags TEXT NOT NULL DEFAULT '[]'")
                .execute(&self.pool)
                .await
        {
            if !is_duplicate_column_error(&e) {
                return Err(e.into());
            }
        }

        // Phase 77.6 — memory_type column for per-type half-life decay.
        // Idempotent: swallow "duplicate column" on re-runs.
        if let Err(e) =
            sqlx::query("ALTER TABLE memories ADD COLUMN memory_type TEXT")
                .execute(&self.pool)
                .await
        {
            if !is_duplicate_column_error(&e) {
                return Err(e.into());
            }
        }

        // FTS5 virtual table for full-text search
        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content,
                id UNINDEXED,
                agent_id UNINDEXED
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS interactions (
                id         TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                agent_id   TEXT NOT NULL,
                role       TEXT NOT NULL,
                content    TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_interactions_session
             ON interactions(session_id, created_at DESC)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS reminders (
                id           TEXT PRIMARY KEY,
                agent_id     TEXT NOT NULL,
                session_id   TEXT NOT NULL,
                plugin       TEXT NOT NULL,
                recipient    TEXT NOT NULL,
                message      TEXT NOT NULL,
                due_at       INTEGER NOT NULL,
                claimed_at   INTEGER,
                delivered_at INTEGER,
                created_at   INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        if let Err(e) = sqlx::query("ALTER TABLE reminders ADD COLUMN claimed_at INTEGER")
            .execute(&self.pool)
            .await
        {
            if !is_duplicate_column_error(&e) {
                return Err(e.into());
            }
        }

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_reminders_due
             ON reminders(agent_id, delivered_at, due_at ASC)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS email_followups (
                flow_id          TEXT PRIMARY KEY,
                agent_id         TEXT NOT NULL,
                session_id       TEXT NOT NULL,
                source_plugin    TEXT NOT NULL,
                source_instance  TEXT,
                recipient        TEXT NOT NULL,
                thread_root_id   TEXT NOT NULL,
                instruction      TEXT NOT NULL,
                check_every_secs INTEGER NOT NULL,
                max_attempts     INTEGER NOT NULL,
                attempts         INTEGER NOT NULL DEFAULT 0,
                next_check_at    INTEGER NOT NULL,
                claimed_at       INTEGER,
                status           TEXT NOT NULL DEFAULT 'active',
                status_note      TEXT,
                created_at       INTEGER NOT NULL,
                updated_at       INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_email_followups_due
             ON email_followups(agent_id, status, next_check_at ASC)",
        )
        .execute(&self.pool)
        .await?;

        // Recall-signal tracking (Phase 10.5). One row per memory hit produced
        // by `recall()`. Signals feed the dreaming deep-phase scoring.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS recall_events (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id   TEXT NOT NULL,
                memory_id  TEXT NOT NULL,
                query      TEXT NOT NULL,
                score      REAL NOT NULL,
                ts_ms      INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_recall_events_agent_memory
             ON recall_events(agent_id, memory_id, ts_ms DESC)",
        )
        .execute(&self.pool)
        .await?;

        // Dreaming promotion ledger (Phase 10.6). One row per memory that the
        // deep phase has already promoted to MEMORY.md — prevents double-promotion
        // across sweeps and gives dreaming a quick "have I seen this before" lookup.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS memory_promotions (
                memory_id    TEXT PRIMARY KEY,
                agent_id     TEXT NOT NULL,
                promoted_at  INTEGER NOT NULL,
                score        REAL NOT NULL,
                phase        TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_promotions_agent
             ON memory_promotions(agent_id, promoted_at DESC)",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn remember(
        &self,
        agent_id: &str,
        content: &str,
        tags: &[&str],
    ) -> anyhow::Result<Uuid> {
        self.remember_typed(agent_id, content, tags, None).await
    }

    /// Phase 77.6 — `remember()` variant that also stores the memory type.
    /// When `None`, the column is left NULL (legacy).
    pub async fn remember_typed(
        &self,
        agent_id: &str,
        content: &str,
        tags: &[&str],
        memory_type: Option<MemoryType>,
    ) -> anyhow::Result<Uuid> {
        // Guard: scan for secrets before committing to SQLite.
        let content_to_store = if let Some(ref guard) = self.guard {
            guard
                .check(content)
                .map_err(|e| anyhow::anyhow!("{}", e))?
        } else {
            content.to_string()
        };

        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let tags_json = serde_json::to_string(tags)?;
        let concept_tags = derive_concept_tags("", &content_to_store, MAX_CONCEPT_TAGS);
        let concept_tags_json = serde_json::to_string(&concept_tags)?;
        let memory_type_str = memory_type.map(|t| serde_json::to_string(&t)).transpose()?;
        let now = Utc::now().timestamp_millis();

        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO memories (id, agent_id, content, tags, concept_tags, memory_type, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id_str)
        .bind(agent_id)
        .bind(&content_to_store)
        .bind(&tags_json)
        .bind(&concept_tags_json)
        .bind(&memory_type_str)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        sqlx::query("INSERT INTO memories_fts (content, id, agent_id) VALUES (?, ?, ?)")
            .bind(&content_to_store)
            .bind(&id_str)
            .bind(agent_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        // Phase 36.2 — best-effort mutation event. Fires after the
        // SQLite commit succeeds so subscribers never observe a
        // rolled-back row.
        if let Some(hook) = &self.mutation_hook {
            hook.on_mutation(
                agent_id,
                &self.tenant,
                nexo_driver_types::MemoryMutationScope::SqliteLongTerm,
                nexo_driver_types::MemoryMutationOp::Insert,
                &id_str,
            )
            .await;
        }

        // Phase 5.4 — best-effort embedding: vector path is additive, FTS
        // already covers recall.
        if let Some(provider) = &self.embedding {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                match provider.embed(&[trimmed]).await {
                    Ok(mut vecs) if !vecs.is_empty() => {
                        let v = vecs.remove(0);
                        if v.len() == provider.dimension() {
                            let bytes = vector::pack_f32(&v);
                            if let Err(e) = sqlx::query(
                                "INSERT INTO vec_memories(memory_id, embedding) VALUES (?, ?)",
                            )
                            .bind(&id_str)
                            .bind(bytes)
                            .execute(&self.pool)
                            .await
                            {
                                tracing::warn!(agent=%agent_id, error=%e, "vec insert failed");
                            }
                        } else {
                            tracing::warn!(
                                agent = %agent_id,
                                got = v.len(),
                                expected = provider.dimension(),
                                "embedding dimension mismatch; skipping vector insert"
                            );
                        }
                    }
                    Err(e) => tracing::warn!(agent=%agent_id, error=%e, "embed failed"),
                    Ok(_) => {}
                }
            }
        }

        Ok(id)
    }

    pub async fn recall(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // Phase 10.7: expand the raw query with up to 3 derived concept tags so
        // FTS5 MATCH also hits memories whose stored `content` diverges from
        // the query surface text but shares a known concept.
        let extra_tags = derive_concept_tags("", query, 3);
        self.recall_with_tags(agent_id, query, &extra_tags, limit)
            .await
    }

    /// Same as `recall` but with caller-supplied concept tags OR'd into the
    /// FTS5 match. Empty `extra_tags` → plain query.
    pub async fn recall_with_tags(
        &self,
        agent_id: &str,
        query: &str,
        extra_tags: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let match_expr = build_fts_match(query, extra_tags);

        let rows = sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(
            "SELECT m.id, m.content, m.tags, m.concept_tags, m.memory_type, m.created_at
             FROM memories_fts f
             JOIN memories m ON m.id = f.id
             WHERE f.content MATCH ?
               AND f.agent_id = ?
             ORDER BY rank
             LIMIT ?",
        )
        .bind(&match_expr)
        .bind(agent_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let entries = rows
            .into_iter()
            .map(|(id_str, content, tags_json, concept_tags_json, memory_type_str, ts)| {
                let id = parse_uuid_or_warn(&id_str, "memory.id");
                let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
                let concept_tags: Vec<String> =
                    serde_json::from_str(&concept_tags_json).unwrap_or_default();
                let memory_type = memory_type_str
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .and_then(|s: String| MemoryType::parse(&s));
                let created_at = DateTime::from_timestamp_millis(ts).unwrap_or_else(Utc::now);
                MemoryEntry {
                    id,
                    agent_id: agent_id.to_string(),
                    content,
                    tags,
                    concept_tags,
                    memory_type,
                    created_at,
                }
            })
            .collect();

        Ok(entries)
    }

    /// Replace the concept_tags column for an existing memory row (Phase 10.7).
    /// Used by the dreaming sweep when promoting memories to MEMORY.md.
    pub async fn set_concept_tags(&self, memory_id: Uuid, tags: &[String]) -> anyhow::Result<bool> {
        let json = serde_json::to_string(tags)?;
        let affected = sqlx::query("UPDATE memories SET concept_tags = ? WHERE id = ?")
            .bind(&json)
            .bind(memory_id.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(affected > 0)
    }

    /// Phase 10.8 — number of memories for this agent.
    pub async fn count_memories(&self, agent_id: &str) -> anyhow::Result<u64> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memories WHERE agent_id = ?")
            .bind(agent_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(count.max(0) as u64)
    }

    /// Phase 10.8 — distinct session count across all interactions this agent
    /// has participated in.
    pub async fn count_sessions(&self, agent_id: &str) -> anyhow::Result<u64> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(DISTINCT session_id) FROM interactions WHERE agent_id = ?",
        )
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count.max(0) as u64)
    }

    /// Phase 10.8 — number of memories promoted to MEMORY.md by dreaming.
    pub async fn count_promotions(&self, agent_id: &str) -> anyhow::Result<u64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM memory_promotions WHERE agent_id = ?")
                .bind(agent_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(count.max(0) as u64)
    }

    /// Phase 10.8 — timestamp of the most recent dreaming promotion, or
    /// `None` if no sweep has promoted anything yet.
    pub async fn last_promotion_ts(&self, agent_id: &str) -> anyhow::Result<Option<DateTime<Utc>>> {
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT MAX(promoted_at) FROM memory_promotions WHERE agent_id = ?")
                .bind(agent_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row
            .and_then(|(ts,)| ts)
            .and_then(DateTime::from_timestamp_millis))
    }

    /// Phase 10.8 — number of recall events recorded since `since_ms`
    /// (inclusive).
    pub async fn count_recall_events_since(
        &self,
        agent_id: &str,
        since_ms: i64,
    ) -> anyhow::Result<u64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM recall_events WHERE agent_id = ? AND ts_ms >= ?")
                .bind(agent_id)
                .bind(since_ms)
                .fetch_one(&self.pool)
                .await?;
        Ok(count.max(0) as u64)
    }

    /// Phase 10.8 — top concept tags observed across memories touched by
    /// recall events in the last window. Tally is done in Rust (no JSON1
    /// dependency); join capped at 200 rows.
    pub async fn top_concept_tags_since(
        &self,
        agent_id: &str,
        since_ms: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, u32)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT m.concept_tags
             FROM recall_events r
             JOIN memories m ON m.id = r.memory_id AND m.agent_id = r.agent_id
             WHERE r.agent_id = ? AND r.ts_ms >= ?
             LIMIT 200",
        )
        .bind(agent_id)
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await?;

        let mut tally: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for (tags_json,) in rows {
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
            for t in tags {
                *tally.entry(t).or_insert(0) += 1;
            }
        }
        let mut sorted: Vec<(String, u32)> = tally.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        sorted.truncate(limit);
        Ok(sorted)
    }

    /// Phase 5.4 — semantic recall via sqlite-vec nearest-neighbor.
    /// Returns `Err` if no embedding provider is configured.
    pub async fn recall_vector(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let provider = self.embedding.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "the operator hasn't configured semantic memory; use mode 'keyword' instead"
            )
        })?;
        let trimmed = query.trim();
        if trimmed.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut vecs = provider.embed(&[trimmed]).await?;
        if vecs.is_empty() {
            return Ok(Vec::new());
        }
        let qvec = vecs.remove(0);
        if qvec.len() != provider.dimension() {
            anyhow::bail!(
                "embedding dimension mismatch: got {}, expected {}",
                qvec.len(),
                provider.dimension()
            );
        }
        let bytes = vector::pack_f32(&qvec);
        // vec0 KNN: K is passed via a `k = N` filter in the MATCH clause.
        // We fetch top `limit * 2` so the agent_id filter (post-JOIN) still
        // yields enough rows for multi-tenant databases.
        let k = (limit as i64 * 2).max(limit as i64);
        let rows: Vec<(String, f64)> = sqlx::query_as(
            "SELECT memory_id, distance
             FROM vec_memories
             WHERE embedding MATCH ? AND k = ?
             ORDER BY distance",
        )
        .bind(bytes)
        .bind(k)
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(Vec::new());
        }

        // Hydrate entries preserving the vector-distance order, filtered by
        // agent_id so agents never see each other's memories.
        let mut out: Vec<MemoryEntry> = Vec::new();
        for (id_str, _distance) in rows {
            if out.len() >= limit {
                break;
            }
            let row: Option<(String, String, String, String, Option<String>, i64)> = sqlx::query_as(
                "SELECT id, agent_id, content, tags, memory_type, created_at
                 FROM memories
                 WHERE id = ? AND agent_id = ?",
            )
            .bind(&id_str)
            .bind(agent_id)
            .fetch_optional(&self.pool)
            .await?;
            if let Some((id, aid, content, tags_json, memory_type_str, created_at_ms)) = row {
                let memory_type = memory_type_str
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<String>(s).ok())
                    .and_then(|s| MemoryType::parse(&s));
                out.push(MemoryEntry {
                    id: Uuid::parse_str(&id)?,
                    agent_id: aid,
                    content,
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    concept_tags: Vec::new(),
                    memory_type,
                    created_at: chrono::DateTime::<Utc>::from_timestamp_millis(created_at_ms)
                        .unwrap_or_else(Utc::now),
                });
            }
        }
        Ok(out)
    }

    /// Phase 5.4 — reciprocal-rank-fused FTS + vector recall. If no
    /// embedding provider is configured OR the vector branch errors at
    /// runtime, gracefully falls back to FTS-only.
    pub async fn recall_hybrid(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        const RRF_K: f32 = 60.0;
        let k_fetch = limit.max(1) * 2;
        let fts = self.recall(agent_id, query, k_fetch).await?;
        let vec_result = if self.embedding.is_some() {
            match self.recall_vector(agent_id, query, k_fetch).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(error = %e, "hybrid: vector branch failed; FTS only");
                    return Ok(truncate_entries(fts, limit));
                }
            }
        } else {
            return Ok(truncate_entries(fts, limit));
        };

        let mut scores: std::collections::HashMap<Uuid, (f32, MemoryEntry)> =
            std::collections::HashMap::new();
        for (rank, m) in fts.into_iter().enumerate() {
            let id = m.id;
            let entry = scores.entry(id).or_insert((0.0, m));
            entry.0 += 1.0 / (RRF_K + rank as f32 + 1.0);
        }
        for (rank, m) in vec_result.into_iter().enumerate() {
            let id = m.id;
            let entry = scores.entry(id).or_insert((0.0, m));
            entry.0 += 1.0 / (RRF_K + rank as f32 + 1.0);
        }
        let mut ranked: Vec<(f32, MemoryEntry)> = scores.into_values().collect();
        ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(ranked.into_iter().take(limit).map(|(_, m)| m).collect())
    }

    /// Phase 77.6 — find relevant memories with composite scoring:
    /// similarity × recency (per-type half-life) × log1p(frequency),
    /// filtered by already-surfaced dedup and annotated with staleness
    /// warnings.
    ///
    /// Returns up to `limit` [`ScoredMemory`] entries, sorted by score desc.
    pub async fn find_relevant(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
        already_surfaced: &std::collections::HashSet<Uuid>,
        freshness_threshold_days: u32,
    ) -> anyhow::Result<Vec<crate::relevance::ScoredMemory>> {
        use crate::relevance::{freshness_note, score_memories, ScoredMemory};

        let now = Utc::now();
        let fetch_limit = (limit.max(1) * 3).min(30); // oversample then trim
        let candidates = self.recall_hybrid(agent_id, query, fetch_limit).await?;

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        // Build RRF-like similarity scores from hybrid ranking.
        // Top-ranked entries get higher similarity.
        let similarity_scores: Vec<(f32, MemoryEntry)> = candidates
            .iter()
            .enumerate()
            .map(|(rank, entry)| {
                let sim = 1.0 / (1.0 + rank as f32 * 0.1); // decay by position
                (sim, entry.clone())
            })
            .collect();

        // Fetch frequency counts for these candidates from recall_events.
        let entry_refs: Vec<&MemoryEntry> = candidates.iter().collect();
        let frequency_counts = self.frequency_counts_for(agent_id, &entry_refs).await?;

        let scored = score_memories(candidates, &similarity_scores, now, &frequency_counts);

        let mut results: Vec<ScoredMemory> = Vec::with_capacity(scored.len());
        for (score, entry) in scored {
            if already_surfaced.contains(&entry.id) {
                continue;
            }
            let warning = freshness_note(&entry, now, freshness_threshold_days);
            results.push(ScoredMemory {
                entry,
                score,
                freshness_warning: warning,
            });
            if results.len() >= limit {
                break;
            }
        }

        Ok(results)
    }

    /// Phase 77.6 — fetch recall-event counts for a set of memory entries.
    async fn frequency_counts_for(
        &self,
        agent_id: &str,
        entries: &[&MemoryEntry],
    ) -> anyhow::Result<std::collections::HashMap<Uuid, u32>> {
        let mut counts = std::collections::HashMap::new();
        if entries.is_empty() {
            return Ok(counts);
        }

        // Build IN clause placeholders manually instead of using itertools join.
        let placeholders: Vec<String> = entries.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT memory_id, COUNT(*) as cnt
             FROM recall_events
             WHERE agent_id = ? AND memory_id IN ({})
             GROUP BY memory_id",
            placeholders.join(",")
        );

        let mut query = sqlx::query_as::<_, (String, i64)>(&sql)
            .bind(agent_id);
        for entry in entries {
            query = query.bind(entry.id.to_string());
        }
        let rows = query.fetch_all(&self.pool).await?;

        for (id_str, cnt) in rows {
            if let Ok(uuid) = Uuid::parse_str(&id_str) {
                counts.insert(uuid, cnt.max(0) as u32);
            }
        }
        Ok(counts)
    }

    pub async fn forget(&self, id: Uuid) -> anyhow::Result<bool> {
        let id_str = id.to_string();
        let mut tx = self.pool.begin().await?;

        // Capture agent_id pre-DELETE so the mutation event (fired
        // post-commit) carries it. `None` means the row was already
        // gone — DELETE returns 0 rows and we skip the event.
        let agent_id: Option<(String,)> =
            sqlx::query_as("SELECT agent_id FROM memories WHERE id = ?")
                .bind(&id_str)
                .fetch_optional(&mut *tx)
                .await?;

        let rows = sqlx::query("DELETE FROM memories WHERE id = ?")
            .bind(&id_str)
            .execute(&mut *tx)
            .await?
            .rows_affected();

        sqlx::query("DELETE FROM memories_fts WHERE id = ?")
            .bind(&id_str)
            .execute(&mut *tx)
            .await?;

        // Audit fix: also purge the vector row. Previously, forgetting
        // a memory left its embedding in `vec_memories` — recall_vector
        // would return an orphan memory_id whose JOIN against `memories`
        // yielded null content, contaminating downstream consumers.
        // The DELETE no-ops when vector mode is disabled (table absent
        // won't trip because migrate() only creates it when an
        // embedding provider is configured — so we guard by checking
        // the embedding option).
        if self.embedding.is_some() {
            sqlx::query("DELETE FROM vec_memories WHERE memory_id = ?")
                .bind(&id_str)
                .execute(&mut *tx)
                .await?;
        }

        // Also purge recall_events + memory_promotions rows that reference
        // this id so counts don't drift. Foreign-key CASCADE isn't set up
        // on these tables (SQLite requires `PRAGMA foreign_keys=ON` plus
        // explicit FK columns, which we didn't declare), so we delete
        // manually.
        sqlx::query("DELETE FROM recall_events WHERE memory_id = ?")
            .bind(&id_str)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM memory_promotions WHERE memory_id = ?")
            .bind(&id_str)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        // Phase 36.2 — best-effort mutation event after commit.
        if rows > 0 {
            if let (Some(hook), Some((agent_id,))) = (&self.mutation_hook, agent_id) {
                hook.on_mutation(
                    &agent_id,
                    &self.tenant,
                    nexo_driver_types::MemoryMutationScope::SqliteLongTerm,
                    nexo_driver_types::MemoryMutationOp::Delete,
                    &id_str,
                )
                .await;
            }
        }

        Ok(rows > 0)
    }

    pub async fn save_interaction(
        &self,
        session_id: Uuid,
        agent_id: &str,
        role: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp_millis();

        sqlx::query(
            "INSERT INTO interactions (id, session_id, agent_id, role, content, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(session_id.to_string())
        .bind(agent_id)
        .bind(role)
        .bind(content)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn load_interactions(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<StoredInteraction>> {
        let rows = sqlx::query_as::<_, (String, String, String, String, i64)>(
            "SELECT id, agent_id, role, content, created_at
             FROM interactions
             WHERE session_id = ?
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .bind(session_id.to_string())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        // Reverse so oldest-first
        let mut entries: Vec<StoredInteraction> = rows
            .into_iter()
            .map(|(id_str, agent_id, role, content, ts)| {
                let id = parse_uuid_or_warn(&id_str, "memory.id");
                let created_at = DateTime::from_timestamp_millis(ts).unwrap_or_else(Utc::now);
                StoredInteraction {
                    id,
                    session_id,
                    agent_id,
                    role,
                    content,
                    created_at,
                }
            })
            .collect();

        entries.reverse();
        Ok(entries)
    }

    pub async fn schedule_reminder(
        &self,
        agent_id: &str,
        session_id: Uuid,
        plugin: &str,
        recipient: &str,
        message: &str,
        due_at: DateTime<Utc>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let now = Utc::now().timestamp_millis();

        sqlx::query(
            "INSERT INTO reminders (
                id, agent_id, session_id, plugin, recipient, message, due_at, claimed_at, delivered_at, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)"
        )
        .bind(id.to_string())
        .bind(agent_id)
        .bind(session_id.to_string())
        .bind(plugin)
        .bind(recipient)
        .bind(message)
        .bind(due_at.timestamp_millis())
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(id)
    }

    pub async fn list_due_reminders(
        &self,
        agent_id: &str,
        now: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<ReminderEntry>> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String, i64, Option<i64>, Option<i64>, i64)>(
            "SELECT id, session_id, plugin, recipient, message, due_at, claimed_at, delivered_at, created_at
             FROM reminders
             WHERE agent_id = ?
               AND delivered_at IS NULL
               AND due_at <= ?
             ORDER BY due_at ASC
             LIMIT ?"
        )
        .bind(agent_id)
        .bind(now.timestamp_millis())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let entries = rows
            .into_iter()
            .map(
                |(
                    id_str,
                    session_id_str,
                    plugin,
                    recipient,
                    message,
                    due_at,
                    claimed_at,
                    delivered_at,
                    created_at,
                )| {
                    ReminderEntry {
                        id: parse_uuid_or_warn(&id_str, "memory.id"),
                        agent_id: agent_id.to_string(),
                        session_id: Uuid::parse_str(&session_id_str)
                            .unwrap_or_else(|_| Uuid::nil()),
                        plugin,
                        recipient,
                        message,
                        due_at: DateTime::from_timestamp_millis(due_at).unwrap_or_else(Utc::now),
                        claimed_at: claimed_at.and_then(DateTime::from_timestamp_millis),
                        delivered_at: delivered_at.and_then(DateTime::from_timestamp_millis),
                        created_at: DateTime::from_timestamp_millis(created_at)
                            .unwrap_or_else(Utc::now),
                    }
                },
            )
            .collect();

        Ok(entries)
    }

    pub async fn mark_reminder_delivered(&self, id: Uuid) -> anyhow::Result<bool> {
        let updated = sqlx::query(
            "UPDATE reminders
             SET claimed_at = NULL,
                 delivered_at = ?
             WHERE id = ?
               AND delivered_at IS NULL",
        )
        .bind(Utc::now().timestamp_millis())
        .bind(id.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected();

        Ok(updated > 0)
    }

    pub async fn claim_due_reminders(
        &self,
        agent_id: &str,
        now: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<ReminderEntry>> {
        let mut tx = self.pool.begin().await?;

        let rows = sqlx::query_as::<_, (String, String, String, String, String, i64, i64)>(
            "SELECT id, session_id, plugin, recipient, message, due_at, created_at
             FROM reminders
             WHERE agent_id = ?
               AND delivered_at IS NULL
               AND claimed_at IS NULL
               AND due_at <= ?
             ORDER BY due_at ASC
             LIMIT ?",
        )
        .bind(agent_id)
        .bind(now.timestamp_millis())
        .bind(limit as i64)
        .fetch_all(&mut *tx)
        .await?;

        if rows.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let claim_ts = Utc::now().timestamp_millis();
        let mut entries = Vec::with_capacity(rows.len());
        for (id_str, session_id_str, plugin, recipient, message, due_at, created_at) in rows {
            let updated = sqlx::query(
                "UPDATE reminders
                 SET claimed_at = ?
                 WHERE id = ?
                   AND delivered_at IS NULL
                   AND claimed_at IS NULL",
            )
            .bind(claim_ts)
            .bind(&id_str)
            .execute(&mut *tx)
            .await?
            .rows_affected();

            if updated == 0 {
                continue;
            }

            entries.push(ReminderEntry {
                id: parse_uuid_or_warn(&id_str, "memory.id"),
                agent_id: agent_id.to_string(),
                session_id: Uuid::parse_str(&session_id_str).unwrap_or_else(|_| Uuid::nil()),
                plugin,
                recipient,
                message,
                due_at: DateTime::from_timestamp_millis(due_at).unwrap_or_else(Utc::now),
                claimed_at: DateTime::from_timestamp_millis(claim_ts),
                delivered_at: None,
                created_at: DateTime::from_timestamp_millis(created_at).unwrap_or_else(Utc::now),
            });
        }

        tx.commit().await?;
        Ok(entries)
    }

    /// Drop `recall_events` rows older than `retain_days`. Intended to run
    /// from the dreaming phase or a periodic heartbeat — the raw event
    /// log accumulates a row per memory hit and grows without a policy,
    /// eventually slowing the `count_recall_events_since` / `recalled_
    /// memories` queries that iterate over it. Default retention should
    /// be generous (30d keeps the rolling-window features of the signal
    /// scorer working) but bounded.
    pub async fn prune_recall_events(&self, retain_days: i64) -> anyhow::Result<u64> {
        let cutoff = Utc::now().timestamp_millis() - retain_days.saturating_mul(86_400_000);
        let result = sqlx::query("DELETE FROM recall_events WHERE ts_ms < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// Force a WAL checkpoint — flushes the -wal file back into the main
    /// db. Call periodically under heavy write load so the WAL doesn't
    /// grow beyond disk / backup expectations. Safe to call at any time;
    /// NOOP when journal_mode isn't WAL.
    pub async fn wal_checkpoint(&self) -> anyhow::Result<()> {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn release_reminder_claim(&self, id: Uuid) -> anyhow::Result<bool> {
        let updated = sqlx::query(
            "UPDATE reminders
             SET claimed_at = NULL
             WHERE id = ?
               AND delivered_at IS NULL",
        )
        .bind(id.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected();

        Ok(updated > 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_email_followup(
        &self,
        flow_id: Uuid,
        agent_id: &str,
        session_id: Uuid,
        source_plugin: &str,
        source_instance: Option<&str>,
        recipient: &str,
        thread_root_id: &str,
        instruction: &str,
        check_every_secs: u64,
        max_attempts: u32,
        next_check_at: DateTime<Utc>,
    ) -> anyhow::Result<(EmailFollowupEntry, bool)> {
        if let Some(existing) = self.get_email_followup(flow_id).await? {
            return Ok((existing, false));
        }
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO email_followups (
                flow_id, agent_id, session_id, source_plugin, source_instance, recipient,
                thread_root_id, instruction, check_every_secs, max_attempts, attempts,
                next_check_at, claimed_at, status, status_note, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, NULL, 'active', NULL, ?, ?)",
        )
        .bind(flow_id.to_string())
        .bind(agent_id)
        .bind(session_id.to_string())
        .bind(source_plugin)
        .bind(source_instance.map(str::to_string))
        .bind(recipient)
        .bind(thread_root_id)
        .bind(instruction)
        .bind(check_every_secs as i64)
        .bind(max_attempts as i64)
        .bind(next_check_at.timestamp_millis())
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        let created = self
            .get_email_followup(flow_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("email_followup inserted but not found"))?;
        Ok((created, true))
    }

    pub async fn get_email_followup(
        &self,
        flow_id: Uuid,
    ) -> anyhow::Result<Option<EmailFollowupEntry>> {
        let row: Option<EmailFollowupDbRow> = sqlx::query_as(
            "SELECT flow_id, agent_id, session_id, source_plugin, source_instance, recipient,
                    thread_root_id, instruction, check_every_secs, max_attempts, attempts,
                    next_check_at, claimed_at, status, status_note, created_at, updated_at
             FROM email_followups
             WHERE flow_id = ?",
        )
        .bind(flow_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(email_followup_from_row))
    }

    pub async fn cancel_email_followup(
        &self,
        flow_id: Uuid,
        note: Option<&str>,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp_millis();
        let updated = sqlx::query(
            "UPDATE email_followups
             SET status = 'cancelled',
                 status_note = ?,
                 claimed_at = NULL,
                 updated_at = ?
             WHERE flow_id = ?
               AND status = 'active'",
        )
        .bind(note.map(str::to_string))
        .bind(now)
        .bind(flow_id.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated > 0 {
            return Ok(true);
        }

        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM email_followups WHERE flow_id = ?")
                .bind(flow_id.to_string())
                .fetch_optional(&self.pool)
                .await?;
        Ok(matches!(
            row.as_ref().map(|r| r.0.as_str()),
            Some("cancelled")
        ))
    }

    pub async fn claim_due_email_followups(
        &self,
        agent_id: &str,
        now: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<EmailFollowupEntry>> {
        let mut tx = self.pool.begin().await?;
        let rows: Vec<EmailFollowupDbRow> = sqlx::query_as(
            "SELECT flow_id, agent_id, session_id, source_plugin, source_instance, recipient,
                    thread_root_id, instruction, check_every_secs, max_attempts, attempts,
                    next_check_at, claimed_at, status, status_note, created_at, updated_at
             FROM email_followups
             WHERE agent_id = ?
               AND status = 'active'
               AND claimed_at IS NULL
               AND next_check_at <= ?
             ORDER BY next_check_at ASC
             LIMIT ?",
        )
        .bind(agent_id)
        .bind(now.timestamp_millis())
        .bind(limit as i64)
        .fetch_all(&mut *tx)
        .await?;

        if rows.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let claim_ts = Utc::now().timestamp_millis();
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let flow_id = row.flow_id.clone();
            let updated = sqlx::query(
                "UPDATE email_followups
                 SET claimed_at = ?,
                     updated_at = ?
                 WHERE flow_id = ?
                   AND status = 'active'
                   AND claimed_at IS NULL",
            )
            .bind(claim_ts)
            .bind(claim_ts)
            .bind(&flow_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if updated == 0 {
                continue;
            }
            let mut entry = email_followup_from_row(row);
            entry.claimed_at = DateTime::from_timestamp_millis(claim_ts);
            entry.updated_at = DateTime::from_timestamp_millis(claim_ts).unwrap_or_else(Utc::now);
            out.push(entry);
        }

        tx.commit().await?;
        Ok(out)
    }

    pub async fn requeue_email_followup_after_error(
        &self,
        flow_id: Uuid,
        next_check_at: DateTime<Utc>,
        error: &str,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp_millis();
        let updated = sqlx::query(
            "UPDATE email_followups
             SET claimed_at = NULL,
                 next_check_at = ?,
                 status_note = ?,
                 updated_at = ?
             WHERE flow_id = ?
               AND status = 'active'",
        )
        .bind(next_check_at.timestamp_millis())
        .bind(error.to_string())
        .bind(now)
        .bind(flow_id.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(updated > 0)
    }

    pub async fn advance_email_followup_attempt(
        &self,
        flow_id: Uuid,
        next_check_at: Option<DateTime<Utc>>,
        note: Option<&str>,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp_millis();
        let (status, next_ms) = match next_check_at {
            Some(ts) => ("active", ts.timestamp_millis()),
            None => ("exhausted", now),
        };
        let updated = sqlx::query(
            "UPDATE email_followups
             SET attempts = attempts + 1,
                 claimed_at = NULL,
                 next_check_at = ?,
                 status = ?,
                 status_note = ?,
                 updated_at = ?
             WHERE flow_id = ?
               AND status = 'active'",
        )
        .bind(next_ms)
        .bind(status)
        .bind(note.map(str::to_string))
        .bind(now)
        .bind(flow_id.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(updated > 0)
    }

    // ── Recall signal tracking (Phase 10.5) ──────────────────────────────────

    /// Record that a memory was surfaced by a search query. Called once per
    /// hit, typically right after `recall()` returns. Score is caller-chosen
    /// (reciprocal rank is a reasonable default).
    pub async fn record_recall_event(
        &self,
        agent_id: &str,
        memory_id: Uuid,
        query: &str,
        score: f32,
    ) -> anyhow::Result<()> {
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO recall_events (agent_id, memory_id, query, score, ts_ms)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(memory_id.to_string())
        .bind(query)
        .bind(score as f64)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Aggregate signals for a single memory. All returned values are
    /// normalized to [0.0, 1.0] so a deep-phase scoring pass can weight them
    /// without further rescaling.
    pub async fn recall_signals(
        &self,
        agent_id: &str,
        memory_id: Uuid,
        now_ms: Option<i64>,
    ) -> anyhow::Result<RecallSignals> {
        let now_ms = now_ms.unwrap_or_else(|| Utc::now().timestamp_millis());
        let rows = sqlx::query_as::<_, (String, f64, i64)>(
            "SELECT query, score, ts_ms
             FROM recall_events
             WHERE agent_id = ? AND memory_id = ?",
        )
        .bind(agent_id)
        .bind(memory_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        // Look up memory_type for per-type half-life. Default Project when missing.
        let half_life = self
            .memory_type_for(memory_id)
            .await
            .unwrap_or(None)
            .map(|t| t.half_life_days())
            .unwrap_or_else(|| MemoryType::Project.half_life_days());

        Ok(aggregate_signals(&rows, now_ms, half_life))
    }

    /// Phase 77.6 — look up the memory_type for a single memory entry.
    async fn memory_type_for(&self, memory_id: Uuid) -> anyhow::Result<Option<MemoryType>> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT memory_type FROM memories WHERE id = ?",
        )
        .bind(memory_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .and_then(|(s,)| s)
            .as_deref()
            .and_then(|s| serde_json::from_str::<String>(s).ok())
            .and_then(|s| MemoryType::parse(&s)))
    }

    /// List every memory for `agent_id` that has at least one recall event.
    /// Returns `(memory_id, content)` tuples — the minimal shape the deep
    /// phase needs to rank + promote candidates.
    pub async fn recalled_memories(&self, agent_id: &str) -> anyhow::Result<Vec<(Uuid, String)>> {
        // Default cap keeps memory bounded on long-lived agents; the
        // dreaming loop still ranks and prunes further downstream. On
        // an agent with millions of recalls the untruncated variant
        // pulled the whole table into heap before scoring.
        self.recalled_memories_capped(agent_id, 10_000).await
    }

    /// Capped variant of [`recalled_memories`]. Orders by recall count
    /// descending so the most-recalled candidates survive truncation.
    pub async fn recalled_memories_capped(
        &self,
        agent_id: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, String)>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT m.id, m.content
             FROM memories m
             JOIN recall_events r ON r.memory_id = m.id AND r.agent_id = m.agent_id
             WHERE m.agent_id = ?
             GROUP BY m.id, m.content
             ORDER BY COUNT(r.id) DESC
             LIMIT ?",
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, content)| Uuid::parse_str(&id).ok().map(|u| (u, content)))
            .collect())
    }

    /// Record a successful deep-phase promotion so later sweeps skip it.
    pub async fn mark_promoted(
        &self,
        agent_id: &str,
        memory_id: Uuid,
        score: f32,
        phase: &str,
    ) -> anyhow::Result<()> {
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT OR REPLACE INTO memory_promotions
             (memory_id, agent_id, promoted_at, score, phase)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(memory_id.to_string())
        .bind(agent_id)
        .bind(now)
        .bind(score as f64)
        .bind(phase)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// True when the memory has already been promoted by a prior sweep.
    pub async fn is_promoted(&self, memory_id: Uuid) -> anyhow::Result<bool> {
        let row = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM memory_promotions WHERE memory_id = ?",
        )
        .bind(memory_id.to_string())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0 > 0)
    }

    /// Bulk variant of `recall_signals` — returns a map keyed by memory_id.
    /// Useful for dreaming's deep phase which ranks many candidates at once.
    pub async fn recall_signals_for_agent(
        &self,
        agent_id: &str,
        now_ms: Option<i64>,
    ) -> anyhow::Result<std::collections::HashMap<Uuid, RecallSignals>> {
        let now_ms = now_ms.unwrap_or_else(|| Utc::now().timestamp_millis());
        let rows = sqlx::query_as::<_, (String, String, f64, i64)>(
            "SELECT memory_id, query, score, ts_ms
             FROM recall_events
             WHERE agent_id = ?",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?;

        let mut by_id: std::collections::HashMap<Uuid, Vec<(String, f64, i64)>> =
            std::collections::HashMap::new();
        for (mid_str, query, score, ts) in rows {
            if let Ok(mid) = Uuid::parse_str(&mid_str) {
                by_id.entry(mid).or_default().push((query, score, ts));
            }
        }

        Ok(by_id
            .into_iter()
            .map(|(mid, events)| {
                // Phase 77.6 — default Project half-life for bulk path.
                // Per-memory type lookup deferred to avoid N+1 queries.
                (mid, aggregate_signals(&events, now_ms, MemoryType::Project.half_life_days()))
            })
            .collect())
    }
}

/// Build an FTS5 MATCH expression from a raw query plus optional concept tags.
/// Each term is wrapped in double quotes and FTS5-escaped so user input cannot
/// break out into operator syntax. Tags are OR'd onto the end.
/// Parse a stored UUID string, falling back to a fresh Uuid on corruption
/// but logging a warn so operators can detect DB corruption instead of
/// silently renaming rows. The old `unwrap_or_else(|_| Uuid::new_v4())`
/// pattern invented a new id for every corrupted row on every read,
/// making the entries permanently unreachable via their real id.
/// Heuristic: SQLite reports "duplicate column name: X" when ALTER TABLE
/// re-adds an already-present column. We treat that as expected on
/// startup migrations and bubble everything else up.
fn is_duplicate_column_error(e: &sqlx::Error) -> bool {
    e.to_string()
        .to_ascii_lowercase()
        .contains("duplicate column")
}

fn email_followup_from_row(row: EmailFollowupDbRow) -> EmailFollowupEntry {
    let (
        flow_id,
        agent_id,
        session_id,
        source_plugin,
        source_instance,
        recipient,
        thread_root_id,
        instruction,
        check_every_secs,
        max_attempts,
        attempts,
        next_check_at,
        claimed_at,
        status,
        status_note,
        created_at,
        updated_at,
    ) = (
        row.flow_id,
        row.agent_id,
        row.session_id,
        row.source_plugin,
        row.source_instance,
        row.recipient,
        row.thread_root_id,
        row.instruction,
        row.check_every_secs,
        row.max_attempts,
        row.attempts,
        row.next_check_at,
        row.claimed_at,
        row.status,
        row.status_note,
        row.created_at,
        row.updated_at,
    );
    EmailFollowupEntry {
        flow_id: parse_uuid_or_warn(&flow_id, "email_followups.flow_id"),
        agent_id,
        session_id: Uuid::parse_str(&session_id).unwrap_or_else(|_| Uuid::nil()),
        source_plugin,
        source_instance,
        recipient,
        thread_root_id,
        instruction,
        check_every_secs: check_every_secs.max(0) as u64,
        max_attempts: max_attempts.clamp(0, i64::from(u32::MAX)) as u32,
        attempts: attempts.clamp(0, i64::from(u32::MAX)) as u32,
        next_check_at: DateTime::from_timestamp_millis(next_check_at).unwrap_or_else(Utc::now),
        claimed_at: claimed_at.and_then(DateTime::from_timestamp_millis),
        status: EmailFollowupStatus::from_db(&status),
        status_note,
        created_at: DateTime::from_timestamp_millis(created_at).unwrap_or_else(Utc::now),
        updated_at: DateTime::from_timestamp_millis(updated_at).unwrap_or_else(Utc::now),
    }
}

fn parse_uuid_or_warn(raw: &str, context: &str) -> Uuid {
    match Uuid::parse_str(raw) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                raw,
                context,
                error = %e,
                "memory: corrupted UUID in db row — substituting a fresh id for this read"
            );
            Uuid::new_v4()
        }
    }
}

fn build_fts_match(query: &str, extra_tags: &[String]) -> String {
    let mut parts = Vec::with_capacity(1 + extra_tags.len());
    let q = fts_quote(query);
    if !q.is_empty() {
        parts.push(q);
    }
    for tag in extra_tags {
        let t = fts_quote(tag);
        if t.is_empty() {
            continue;
        }
        if !parts.contains(&t) {
            parts.push(t);
        }
    }
    if parts.is_empty() {
        // FTS5 MATCH cannot be empty; fall back to a sentinel that matches
        // nothing. Caller returns an empty result set either way.
        return "\"\"".to_string();
    }
    parts.join(" OR ")
}

/// Escape a term for an FTS5 MATCH phrase. Double quotes inside the term are
/// doubled (FTS5 escape rule), then the whole term is wrapped in double quotes
/// so operators (`AND`, `*`, `(`…) in user input are treated as literals.
fn fts_quote(term: &str) -> String {
    let trimmed = term.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let escaped = trimmed.replace('"', "\"\"");
    format!("\"{}\"", escaped)
}

/// Aggregated recall signals for one memory entry.
/// Ranges documented per field; all in [0.0, 1.0] unless noted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecallSignals {
    /// Number of distinct recall events (raw count, then log-normalized).
    pub frequency: f32,
    /// Mean of the scores recorded with each event.
    pub relevance: f32,
    /// Exponential decay based on days since the most recent hit.
    pub recency: f32,
    /// Distinct query strings / distinct days — proxy for "surfaced in many
    /// different contexts". Max 1.0 at >=5 distinct.
    pub diversity: f32,
    /// Raw count kept alongside the normalized frequency — dreaming gates
    /// like `minRecallCount` need the raw number.
    pub recall_count: u32,
    /// Number of distinct UTC days on which the memory was surfaced.
    /// Used as a tiebreaker / `consolidation` proxy.
    pub unique_days: u32,
}

impl Default for RecallSignals {
    fn default() -> Self {
        Self {
            frequency: 0.0,
            relevance: 0.0,
            recency: 0.0,
            diversity: 0.0,
            recall_count: 0,
            unique_days: 0,
        }
    }
}

fn aggregate_signals(events: &[(String, f64, i64)], now_ms: i64, half_life_days: f64) -> RecallSignals {
    if events.is_empty() {
        return RecallSignals::default();
    }

    let count = events.len() as u32;
    // log1p normalization: saturates gracefully — 10 hits ≈ 0.83, 100 hits → 1.0.
    let freq_norm = ((count as f32).ln_1p() / 6.0_f32.ln_1p()).min(1.0);

    let sum_score: f64 = events.iter().map(|(_, s, _)| *s).sum();
    let relevance = (sum_score / events.len() as f64) as f32;
    let relevance = relevance.clamp(0.0, 1.0);

    let latest_ts = events.iter().map(|(_, _, ts)| *ts).max().unwrap_or(now_ms);
    let days_since = ((now_ms - latest_ts).max(0) as f64) / (1000.0 * 60.0 * 60.0 * 24.0);
    // Phase 77.6 — per-type half-life from MemoryType instead of hardcoded 7.0.
    // half_life_days = 0 → instant decay (recency = 0.0).
    let recency = if half_life_days <= 0.0 {
        0.0
    } else {
        (-(days_since) * std::f64::consts::LN_2 / half_life_days).exp() as f32
    };

    let mut distinct_queries: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut distinct_days: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for (q, _, ts) in events {
        distinct_queries.insert(q.as_str());
        distinct_days.insert(ts / (1000 * 60 * 60 * 24));
    }
    let unique_days = distinct_days.len() as u32;
    let diversity_raw = distinct_queries.len().max(distinct_days.len()) as f32;
    let diversity = (diversity_raw / 5.0).min(1.0);

    RecallSignals {
        frequency: freq_norm,
        relevance,
        recency,
        diversity,
        recall_count: count,
        unique_days,
    }
}

fn truncate_entries(mut entries: Vec<MemoryEntry>, limit: usize) -> Vec<MemoryEntry> {
    entries.truncate(limit);
    entries
}

#[cfg(test)]
mod recall_signal_tests {
    use super::*;

    fn evt(q: &str, score: f64, ts_ms: i64) -> (String, f64, i64) {
        (q.to_string(), score, ts_ms)
    }

    fn day_ms(days: i64) -> i64 {
        days * 24 * 60 * 60 * 1000
    }

    #[test]
    fn empty_events_yield_zero_signals() {
        let s = aggregate_signals(&[], 1_000_000, 90.0);
        assert_eq!(s.frequency, 0.0);
        assert_eq!(s.recall_count, 0);
    }

    #[test]
    fn frequency_log_normalizes_monotonically() {
        let now = day_ms(10);
        let ev1 = vec![evt("q", 1.0, now)];
        let ev5 = vec![evt("q", 1.0, now); 5];
        let ev50 = vec![evt("q", 1.0, now); 50];
        let s1 = aggregate_signals(&ev1, now, 90.0);
        let s5 = aggregate_signals(&ev5, now, 90.0);
        let s50 = aggregate_signals(&ev50, now, 90.0);
        assert!(s1.frequency < s5.frequency);
        assert!(s5.frequency < s50.frequency);
        assert!(s50.frequency <= 1.0);
    }

    #[test]
    fn recency_decays_with_distance() {
        let now = day_ms(30);
        // Use 7.0 day half-life for this test to verify the decay formula
        // with a short, observable half-life.
        let fresh = aggregate_signals(&[evt("q", 1.0, now)], now, 7.0);
        let week_old = aggregate_signals(&[evt("q", 1.0, now - day_ms(7))], now, 7.0);
        let month_old = aggregate_signals(&[evt("q", 1.0, now - day_ms(30))], now, 7.0);
        assert!((fresh.recency - 1.0).abs() < 1e-4);
        assert!(
            (week_old.recency - 0.5).abs() < 1e-2,
            "7d half-life ≈ 0.5, got {}",
            week_old.recency
        );
        assert!(month_old.recency < 0.1);
    }

    #[test]
    fn diversity_counts_distinct_queries_and_days() {
        let now = day_ms(30);
        let same = aggregate_signals(
            &[evt("q", 1.0, now), evt("q", 1.0, now), evt("q", 1.0, now)],
            now,
            90.0,
        );
        let varied = aggregate_signals(
            &[
                evt("q1", 1.0, now - day_ms(0)),
                evt("q2", 1.0, now - day_ms(1)),
                evt("q3", 1.0, now - day_ms(2)),
                evt("q4", 1.0, now - day_ms(3)),
                evt("q5", 1.0, now - day_ms(4)),
            ],
            now,
            90.0,
        );
        assert!(varied.diversity > same.diversity);
        assert_eq!(varied.unique_days, 5);
        assert!((varied.diversity - 1.0).abs() < 1e-4);
    }

    #[test]
    fn relevance_is_mean_of_scores() {
        let now = day_ms(1);
        let s = aggregate_signals(
            &[evt("q", 0.2, now), evt("q", 0.8, now), evt("q", 0.5, now)],
            now,
            90.0,
        );
        assert!((s.relevance - 0.5).abs() < 1e-4);
    }
}
