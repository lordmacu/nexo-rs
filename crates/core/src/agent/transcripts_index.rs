//! SQLite FTS5 index over transcript content. Source of truth stays
//! the JSONL files written by [`super::transcripts::TranscriptWriter`];
//! this index is derivable and can be rebuilt from disk via
//! [`TranscriptsIndex::rebuild_for_agent`].
//!
//! The index is shared across agents — cross-agent isolation is
//! enforced by the `agent_id` filter on every query.

use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

use super::transcripts::{TranscriptLine, TranscriptWriter};

#[derive(Debug, Clone)]
pub struct IndexedHit {
    pub session_id: Uuid,
    pub agent_id: String,
    pub timestamp_unix: i64,
    pub role: String,
    pub source_plugin: String,
    /// FTS5 `snippet()` excerpt, with `[`/`]` markers around matched
    /// terms. Capped at ~120 chars.
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct TranscriptsIndex {
    pool: SqlitePool,
}

impl TranscriptsIndex {
    /// Open or create the index DB at `path`. Idempotent.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let url = format!("sqlite://{}", path.display());
        let opts = SqliteConnectOptions::from_str(&url)
            .with_context(|| format!("invalid sqlite url for {}", path.display()))?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .with_context(|| format!("opening transcripts index at {}", path.display()))?;
        // FTS5 virtual table — agent_id et al. are UNINDEXED so they
        // ride along but aren't tokenized.
        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS transcripts_fts USING fts5(
                content,
                agent_id        UNINDEXED,
                session_id      UNINDEXED,
                timestamp_unix  UNINDEXED,
                role            UNINDEXED,
                source_plugin   UNINDEXED,
                tokenize = 'unicode61 remove_diacritics 2'
            )",
        )
        .execute(&pool)
        .await
        .context("creating transcripts_fts table")?;
        Ok(Self { pool })
    }

    pub async fn insert(
        &self,
        agent_id: &str,
        session_id: Uuid,
        timestamp_unix: i64,
        role: &str,
        source_plugin: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO transcripts_fts (content, agent_id, session_id, timestamp_unix, role, source_plugin) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(content)
        .bind(agent_id)
        .bind(session_id.to_string())
        .bind(timestamp_unix)
        .bind(role)
        .bind(source_plugin)
        .execute(&self.pool)
        .await
        .context("inserting into transcripts_fts")?;
        Ok(())
    }

    /// FTS5 MATCH search filtered by `agent_id`. `query` is escaped as
    /// a single phrase so quotes / special chars in user input cannot
    /// inject FTS operators.
    pub async fn search(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<IndexedHit>> {
        let limit = limit.clamp(1, 500) as i64;
        let phrase = escape_fts_phrase(query);
        let rows = sqlx::query(
            "SELECT session_id, agent_id, timestamp_unix, role, source_plugin, \
                    snippet(transcripts_fts, 0, '[', ']', '...', 12) AS snip \
             FROM transcripts_fts \
             WHERE agent_id = ? AND content MATCH ? \
             ORDER BY rank \
             LIMIT ?",
        )
        .bind(agent_id)
        .bind(&phrase)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("querying transcripts_fts")?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let sid: String = row.try_get("session_id")?;
            let session_id = Uuid::parse_str(&sid).unwrap_or_else(|_| Uuid::nil());
            out.push(IndexedHit {
                session_id,
                agent_id: row.try_get("agent_id")?,
                timestamp_unix: row.try_get("timestamp_unix")?,
                role: row.try_get("role")?,
                source_plugin: row.try_get("source_plugin")?,
                snippet: row.try_get("snip")?,
            });
        }
        Ok(out)
    }

    pub async fn count_for_agent(&self, agent_id: &str) -> anyhow::Result<u64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM transcripts_fts WHERE agent_id = ?")
            .bind(agent_id)
            .fetch_one(&self.pool)
            .await
            .context("counting transcripts_fts")?;
        let n: i64 = row.try_get("n")?;
        Ok(n.max(0) as u64)
    }

    /// Wipe and rebuild every row owned by `agent_id` from JSONL files
    /// found under `transcripts_root`. Returns the number of entries
    /// indexed.
    pub async fn rebuild_for_agent(
        &self,
        agent_id: &str,
        transcripts_root: &Path,
    ) -> anyhow::Result<usize> {
        // Walk the JSONL files first (read-only; safe outside the
        // transaction). Then DELETE + INSERT inside a single
        // transaction so a crash mid-rebuild leaves the index
        // either fully old or fully new — never half-empty.
        let writer = TranscriptWriter::new(PathBuf::from(transcripts_root), agent_id.to_string());
        let mut entries = match tokio::fs::read_dir(transcripts_root).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Empty root → just clear the index for this agent.
                sqlx::query("DELETE FROM transcripts_fts WHERE agent_id = ?")
                    .bind(agent_id)
                    .execute(&self.pool)
                    .await
                    .context("wiping rows for agent")?;
                return Ok(0);
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "read transcripts_root {}: {e}",
                    transcripts_root.display()
                ))
            }
        };

        // Collect everything we want to insert before touching the DB.
        struct Row {
            sid: Uuid,
            ts: i64,
            role: &'static str,
            source_plugin: String,
            content: String,
        }
        let mut rows: Vec<Row> = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let stem = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let Ok(sid) = Uuid::parse_str(&stem) else {
                continue;
            };
            let lines = writer.read_session(sid).await.unwrap_or_default();
            for line in lines {
                if let TranscriptLine::Entry(e) = line {
                    rows.push(Row {
                        sid,
                        ts: e.timestamp.timestamp(),
                        role: role_str(e.role),
                        source_plugin: e.source_plugin,
                        content: e.content,
                    });
                }
            }
        }

        let mut tx = self.pool.begin().await.context("begin tx")?;
        sqlx::query("DELETE FROM transcripts_fts WHERE agent_id = ?")
            .bind(agent_id)
            .execute(&mut *tx)
            .await
            .context("wiping rows for agent")?;
        let mut indexed = 0usize;
        for row in &rows {
            sqlx::query(
                "INSERT INTO transcripts_fts (content, agent_id, session_id, timestamp_unix, role, source_plugin) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.content)
            .bind(agent_id)
            .bind(row.sid.to_string())
            .bind(row.ts)
            .bind(row.role)
            .bind(&row.source_plugin)
            .execute(&mut *tx)
            .await
            .context("insert row in rebuild")?;
            indexed += 1;
        }
        tx.commit().await.context("commit rebuild tx")?;
        Ok(indexed)
    }
}

fn role_str(role: super::transcripts::TranscriptRole) -> &'static str {
    use super::transcripts::TranscriptRole as R;
    match role {
        R::User => "user",
        R::Assistant => "assistant",
        R::Tool => "tool",
        R::System => "system",
    }
}

/// Escape a user query into a single FTS5 phrase. Embedded `"` are
/// doubled per FTS5 syntax; the whole result is wrapped in quotes.
/// This keeps user input from injecting FTS operators (`OR`, `NOT`,
/// `:`, etc.).
fn escape_fts_phrase(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('"');
    for ch in q.chars() {
        if ch == '"' {
            out.push('"');
            out.push('"');
        } else {
            out.push(ch);
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::transcripts::{TranscriptEntry, TranscriptRole, TranscriptWriter};
    use chrono::Utc;
    use tempfile::tempdir;

    async fn fresh() -> TranscriptsIndex {
        let dir = tempdir().unwrap().keep();
        let path = dir.join("transcripts.db");
        TranscriptsIndex::open(&path).await.unwrap()
    }

    #[tokio::test]
    async fn open_is_idempotent() {
        let dir = tempdir().unwrap().keep();
        let path = dir.join("transcripts.db");
        TranscriptsIndex::open(&path).await.unwrap();
        TranscriptsIndex::open(&path).await.unwrap();
    }

    #[tokio::test]
    async fn insert_then_search_round_trip() {
        let idx = fresh().await;
        let sid = Uuid::new_v4();
        idx.insert("kate", sid, 1000, "user", "wa", "hola codigo de seguridad")
            .await
            .unwrap();
        idx.insert("kate", sid, 1001, "assistant", "wa", "no comparto eso")
            .await
            .unwrap();
        let hits = idx.search("kate", "codigo", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, sid);
        assert!(hits[0].snippet.contains("[codigo]"));
    }

    #[tokio::test]
    async fn search_filters_by_agent() {
        let idx = fresh().await;
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();
        idx.insert("kate", s1, 1, "user", "wa", "shared word here")
            .await
            .unwrap();
        idx.insert("ana", s2, 2, "user", "wa", "shared word here too")
            .await
            .unwrap();
        let kate_hits = idx.search("kate", "shared", 10).await.unwrap();
        assert_eq!(kate_hits.len(), 1);
        assert_eq!(kate_hits[0].agent_id, "kate");
        let ana_hits = idx.search("ana", "shared", 10).await.unwrap();
        assert_eq!(ana_hits.len(), 1);
        assert_eq!(ana_hits[0].agent_id, "ana");
    }

    #[tokio::test]
    async fn count_for_agent_returns_inserted_count() {
        let idx = fresh().await;
        let sid = Uuid::new_v4();
        for i in 0..3 {
            idx.insert("kate", sid, 100 + i, "user", "wa", "row")
                .await
                .unwrap();
        }
        idx.insert("ana", sid, 200, "user", "wa", "row")
            .await
            .unwrap();
        assert_eq!(idx.count_for_agent("kate").await.unwrap(), 3);
        assert_eq!(idx.count_for_agent("ana").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn search_user_input_with_quotes_is_safe() {
        let idx = fresh().await;
        let sid = Uuid::new_v4();
        idx.insert("kate", sid, 1, "user", "wa", "an OR b")
            .await
            .unwrap();
        // FTS would treat OR as operator without escaping; quoted phrase
        // matches literal "an OR b" only.
        let hits = idx.search("kate", "OR", 10).await.unwrap();
        assert!(hits.iter().all(|h| h.session_id == sid));
    }

    #[tokio::test]
    async fn search_user_input_with_fts_operators_is_safe() {
        // Adversarial inputs: each one would, if naively concatenated
        // into the MATCH expression, change the query semantics. Phrase
        // mode (the wrapping `"`) plus our `"` doubling neutralizes
        // every operator the docs list (NEAR, AND, OR, NOT, `:` field,
        // `^` prefix, parens). The query is treated as a literal
        // phrase against `content`, so none of the inputs below should
        // ever return rows from a different agent or a different
        // content string.
        let idx = fresh().await;
        let sid = Uuid::new_v4();
        idx.insert("kate", sid, 1, "user", "wa", "title: hello world")
            .await
            .unwrap();
        idx.insert("ana", Uuid::new_v4(), 2, "user", "wa", "agent_id: leak")
            .await
            .unwrap();
        for adversarial in [
            "field:value",
            "agent_id:ana",
            "OR NOT AND NEAR",
            "\"injection\"",
            "(hello)",
            "^prefix",
        ] {
            let hits = idx.search("kate", adversarial, 10).await.unwrap();
            for h in &hits {
                assert_eq!(
                    h.agent_id, "kate",
                    "input `{adversarial}` leaked rows from another agent"
                );
            }
        }
    }

    #[tokio::test]
    async fn rebuild_for_agent_indexes_jsonl() {
        let dir = tempdir().unwrap().keep();
        let writer = TranscriptWriter::new(&dir, "kate");
        let sid = Uuid::new_v4();
        for content in ["hola", "que tal", "todo bien"] {
            writer
                .append_entry(
                    sid,
                    TranscriptEntry {
                        timestamp: Utc::now(),
                        role: TranscriptRole::User,
                        content: content.to_string(),
                        message_id: None,
                        source_plugin: "wa".into(),
                        sender_id: None,
                    },
                )
                .await
                .unwrap();
        }
        let idx_path = dir.join("idx.db");
        let idx = TranscriptsIndex::open(&idx_path).await.unwrap();
        let n = idx.rebuild_for_agent("kate", &dir).await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(idx.count_for_agent("kate").await.unwrap(), 3);
        let hits = idx.search("kate", "tal", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
    }
}
