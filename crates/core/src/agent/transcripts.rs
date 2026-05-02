//! Session transcripts — Phase 10.4.
//!
//! Append-only JSONL log per session. Layout modeled on OpenClaw's
//! `agents/<agent>/sessions/<sessionId>.jsonl`:
//!   - First line: `{ "type": "session", "version": N, "id": "...",
//!     "timestamp": "...", "agent_id": "...", "source_plugin": "..." }`
//!   - Subsequent lines: one `Entry` per turn (user/assistant/tool).
//!
//! Writes are append-only and line-oriented. No in-place edits, no rewrites.
//! Concurrent writers to the same session are expected to be rare (one runtime
//! per agent), so we rely on the fs `append` mode rather than a file lock.
//!
//! The writer is intentionally decoupled from `SessionManager` — transcripts
//! are a *record*, not a source of truth. `SessionManager` still owns live
//! history; transcripts are what dreaming (Phase 10.6) will later ingest.
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

use super::agent_events::{AgentEventEmitter, NoopAgentEventEmitter};
use super::redaction::Redactor;
use super::transcripts_index::TranscriptsIndex;
use nexo_tool_meta::admin::agent_events::{
    AgentEventKind, TranscriptRole as WireTranscriptRole,
};
pub const TRANSCRIPT_VERSION: u32 = 1;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptLine {
    Session(SessionHeader),
    Entry(TranscriptEntry),
    /// Phase 77.3 — marks a compact turn boundary in the transcript.
    CompactBoundary {
        uuid: String,
        token_count: u64,
        turn_index: u32,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeader {
    pub version: u32,
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub agent_id: String,
    pub source_plugin: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub timestamp: DateTime<Utc>,
    pub role: TranscriptRole,
    pub content: String,
    /// Id of the inbound message, when this entry came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Uuid>,
    /// Channel/plugin that produced or received this entry.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_plugin: String,
    /// Opaque sender identifier (external user id, peer agent id, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    User,
    Assistant,
    Tool,
    System,
}
pub struct TranscriptWriter {
    root: PathBuf,
    agent_id: String,
    redactor: Arc<Redactor>,
    index: Option<Arc<TranscriptsIndex>>,
    /// Phase 82.11 — optional firehose emitter. Called after
    /// the redactor runs (defense-in-depth: emitted body matches
    /// the persisted body). Default is
    /// [`NoopAgentEventEmitter`] so existing callers stay
    /// byte-identical until they opt in via [`Self::with_emitter`].
    event_emitter: Arc<dyn AgentEventEmitter>,
    /// Per-session locks around the header-creation block so only one
    /// writer writes the Session line, and every other writer waits
    /// for the header to be flushed before opening in append mode.
    header_locks: DashMap<PathBuf, Arc<TokioMutex<()>>>,
    /// Phase 82.11 — per-session monotonic counter handed to the
    /// firehose as `seq`. Counts only `TranscriptLine::Entry`
    /// records — must stay in lockstep with `TranscriptReaderFs`'s
    /// entry-only enumeration so backfill `agent_events/read` and
    /// the live notification stream agree on `seq`.
    entry_seq: DashMap<Uuid, Arc<AtomicU64>>,
    /// Phase 83.8.12.4.b — owning tenant. Stamped on every
    /// `TranscriptAppended` firehose event. `None` for
    /// single-tenant deployments (default) — multi-tenant boot
    /// passes the agent's `tenant_id` through
    /// [`Self::with_tenant_id`].
    tenant_id: Option<String>,
}
impl TranscriptWriter {
    pub fn new(root: impl Into<PathBuf>, agent_id: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            agent_id: agent_id.into(),
            redactor: Arc::new(Redactor::disabled()),
            index: None,
            event_emitter: Arc::new(NoopAgentEventEmitter),
            header_locks: DashMap::new(),
            entry_seq: DashMap::new(),
            tenant_id: None,
        }
    }

    /// Phase 83.8.12.4.b — tag the writer with its owning tenant so
    /// every emitted `TranscriptAppended` event carries `tenant_id`.
    /// Boot wire reads `agent.tenant_id` and threads it through.
    /// `None` is the legacy / single-tenant case (default).
    pub fn with_tenant_id(mut self, tenant_id: Option<String>) -> Self {
        self.tenant_id = tenant_id;
        self
    }
    /// Wrap the writer with a redactor and/or FTS index. Both are
    /// optional — `Redactor::disabled()` and `None` reproduce the
    /// behavior of [`Self::new`].
    pub fn with_extras(
        root: impl Into<PathBuf>,
        agent_id: impl Into<String>,
        redactor: Arc<Redactor>,
        index: Option<Arc<TranscriptsIndex>>,
    ) -> Self {
        Self {
            root: root.into(),
            agent_id: agent_id.into(),
            redactor,
            index,
            event_emitter: Arc::new(NoopAgentEventEmitter),
            header_locks: DashMap::new(),
            entry_seq: DashMap::new(),
            tenant_id: None,
        }
    }

    /// Phase 82.11 — install a firehose emitter. Replaces the
    /// default `NoopAgentEventEmitter`. Call AFTER `with_extras`
    /// (or use the standalone factory from boot wiring).
    pub fn with_emitter(mut self, emitter: Arc<dyn AgentEventEmitter>) -> Self {
        self.event_emitter = emitter;
        self
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Phase 82.13.b.1.3 — owning agent id this writer was
    /// pinned to at construction. The
    /// `TranscriptWriterAppender` adapter (in `nexo-setup`)
    /// uses it for a defense-in-depth check that operator
    /// stamps cannot cross into a different agent's
    /// transcript via the shared writer instance.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }
    fn session_path(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl"))
    }
    /// Append one entry to the session transcript. Writes a header first if
    /// the file doesn't exist yet. When a redactor is active, the entry's
    /// `content` is rewritten before any persistence path sees it. When an
    /// FTS index is wired, the (possibly redacted) content is also inserted
    /// best-effort — JSONL stays the source of truth.
    pub async fn append_entry(
        &self,
        session_id: Uuid,
        mut entry: TranscriptEntry,
    ) -> anyhow::Result<()> {
        if self.redactor.is_active() {
            let report = self.redactor.apply(&entry.content);
            entry.content = report.redacted_text;
            for (label, count) in &report.hits {
                tracing::debug!(
                    agent = %self.agent_id,
                    pattern = %label,
                    count = *count,
                    "transcript redaction applied"
                );
            }
        }

        tokio::fs::create_dir_all(&self.root).await?;
        let path = self.session_path(session_id);

        // Per-session mutex so only one writer enters the header-creation
        // block. The winner writes the header and flushes; every other
        // writer blocks until the header is committed, then skips to the
        // append path. Without this lock, a writer that sees
        // AlreadyExists can open the file in append mode before the
        // winner's header hits disk, producing an entry-then-header file.
        let lock = self
            .header_locks
            .entry(path.clone())
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        match tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
        {
            Ok(mut header_file) => {
                let header = TranscriptLine::Session(SessionHeader {
                    version: TRANSCRIPT_VERSION,
                    id: session_id,
                    timestamp: Utc::now(),
                    agent_id: self.agent_id.clone(),
                    source_plugin: entry.source_plugin.clone(),
                });
                write_jsonl(&mut header_file, &header).await?;
                header_file.flush().await?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Another concurrent writer already wrote the header.
            }
            Err(e) => return Err(e.into()),
        }

        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await?;
        write_jsonl(&mut file, &TranscriptLine::Entry(entry.clone())).await?;
        file.flush().await?;

        if let Some(index) = &self.index {
            let role = match entry.role {
                TranscriptRole::User => "user",
                TranscriptRole::Assistant => "assistant",
                TranscriptRole::Tool => "tool",
                TranscriptRole::System => "system",
            };
            if let Err(e) = index
                .insert(
                    &self.agent_id,
                    session_id,
                    entry.timestamp.timestamp(),
                    role,
                    &entry.source_plugin,
                    &entry.content,
                )
                .await
            {
                tracing::warn!(
                    agent = %self.agent_id,
                    error = %e,
                    "transcripts FTS insert failed; JSONL persisted"
                );
            }
        }

        // Phase 82.11 — fire the firehose AFTER both JSONL +
        // FTS persistence so subscribers never see a frame the
        // backfill RPC can't return. `seq` is per-session +
        // entry-only so it matches `TranscriptReaderFs`'s
        // entry-only enumeration. Default emitter is
        // `NoopAgentEventEmitter` → zero-cost when no firehose
        // is wired.
        let seq = self
            .entry_seq
            .entry(session_id)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .fetch_add(1, Ordering::Relaxed);
        let event = AgentEventKind::TranscriptAppended {
            agent_id: self.agent_id.clone(),
            session_id,
            seq,
            role: map_wire_role(entry.role),
            body: entry.content.clone(),
            sent_at_ms: entry.timestamp.timestamp_millis() as u64,
            sender_id: entry.sender_id.clone(),
            source_plugin: if entry.source_plugin.is_empty() {
                "internal".into()
            } else {
                entry.source_plugin.clone()
            },
            // Phase 83.8.12.4.b — stamped from the writer's
            // own `tenant_id` field (set at boot via
            // `with_tenant_id` from `agent.tenant_id`). `None`
            // for single-tenant deployments.
            tenant_id: self.tenant_id.clone(),
        };
        self.event_emitter.emit(event).await;
        Ok(())
    }
    /// Read every line of a session transcript back into memory. Unknown
    /// lines are skipped so the reader is forward-compatible with future
    /// line variants.
    pub async fn read_session(&self, session_id: Uuid) -> anyhow::Result<Vec<TranscriptLine>> {
        let path = self.session_path(session_id);
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(anyhow::anyhow!("read {}: {e}", path.display())),
        };
        let mut out = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TranscriptLine>(line) {
                Ok(parsed) => out.push(parsed),
                Err(e) => tracing::warn!(
                    path = %path.display(),
                    line = %line,
                    error = %e,
                    "skipping unparseable transcript line"
                ),
            }
        }
        Ok(out)
    }
}
/// Phase 82.11 — bridge `TranscriptRole` (core) ↔ wire role
/// enum used by the firehose. Inline because the enum is tiny
/// and the call is on the hot path.
fn map_wire_role(r: TranscriptRole) -> WireTranscriptRole {
    match r {
        TranscriptRole::User => WireTranscriptRole::User,
        TranscriptRole::Assistant => WireTranscriptRole::Assistant,
        TranscriptRole::Tool => WireTranscriptRole::Tool,
        TranscriptRole::System => WireTranscriptRole::System,
    }
}

async fn write_jsonl<T: Serialize>(file: &mut tokio::fs::File, value: &T) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    file.write_all(&bytes).await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tmp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("transcript-{label}-{}", Uuid::new_v4()))
    }
    fn user_entry(text: &str, plugin: &str) -> TranscriptEntry {
        TranscriptEntry {
            timestamp: Utc::now(),
            role: TranscriptRole::User,
            content: text.to_string(),
            message_id: Some(Uuid::new_v4()),
            source_plugin: plugin.to_string(),
            sender_id: Some("user-1".into()),
        }
    }
    fn assistant_entry(text: &str) -> TranscriptEntry {
        TranscriptEntry {
            timestamp: Utc::now(),
            role: TranscriptRole::Assistant,
            content: text.to_string(),
            message_id: None,
            source_plugin: String::new(),
            sender_id: None,
        }
    }
    #[tokio::test]
    async fn first_append_writes_header_then_entry() -> anyhow::Result<()> {
        let root = tmp_dir("header");
        let writer = TranscriptWriter::new(&root, "kate");
        let session_id = Uuid::new_v4();
        writer
            .append_entry(session_id, user_entry("hola", "whatsapp"))
            .await?;
        let lines = writer.read_session(session_id).await?;
        assert_eq!(lines.len(), 2, "expected header + 1 entry");
        match &lines[0] {
            TranscriptLine::Session(h) => {
                assert_eq!(h.version, TRANSCRIPT_VERSION);
                assert_eq!(h.id, session_id);
                assert_eq!(h.agent_id, "kate");
                assert_eq!(h.source_plugin, "whatsapp");
            }
            _ => panic!("first line must be a Session header"),
        }
        match &lines[1] {
            TranscriptLine::Entry(e) => {
                assert_eq!(e.role, TranscriptRole::User);
                assert_eq!(e.content, "hola");
            }
            _ => panic!("second line must be an Entry"),
        }
        tokio::fs::remove_dir_all(&root).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn subsequent_appends_do_not_rewrite_header() -> anyhow::Result<()> {
        let root = tmp_dir("append-only");
        let writer = TranscriptWriter::new(&root, "kate");
        let session_id = Uuid::new_v4();
        writer
            .append_entry(session_id, user_entry("hola", "whatsapp"))
            .await?;
        writer
            .append_entry(session_id, assistant_entry("hola, cómo estás?"))
            .await?;
        writer
            .append_entry(session_id, user_entry("bien", "whatsapp"))
            .await?;
        let lines = writer.read_session(session_id).await?;
        let headers = lines
            .iter()
            .filter(|l| matches!(l, TranscriptLine::Session(_)))
            .count();
        assert_eq!(headers, 1, "exactly one header expected");
        let entries: Vec<_> = lines
            .iter()
            .filter_map(|l| match l {
                TranscriptLine::Entry(e) => Some(e.content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(entries, vec!["hola", "hola, cómo estás?", "bien"]);
        tokio::fs::remove_dir_all(&root).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn independent_sessions_get_separate_files() -> anyhow::Result<()> {
        let root = tmp_dir("isolation");
        let writer = TranscriptWriter::new(&root, "kate");
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        writer
            .append_entry(a, user_entry("one", "whatsapp"))
            .await?;
        writer
            .append_entry(b, user_entry("two", "telegram"))
            .await?;
        let la = writer.read_session(a).await?;
        let lb = writer.read_session(b).await?;
        assert_eq!(la.len(), 2);
        assert_eq!(lb.len(), 2);
        // Cross-contamination check.
        for l in &la {
            if let TranscriptLine::Entry(e) = l {
                assert_eq!(e.content, "one");
            }
        }
        for l in &lb {
            if let TranscriptLine::Entry(e) = l {
                assert_eq!(e.content, "two");
            }
        }
        tokio::fs::remove_dir_all(&root).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn missing_session_reads_as_empty() -> anyhow::Result<()> {
        let root = tmp_dir("missing");
        let writer = TranscriptWriter::new(&root, "kate");
        let lines = writer.read_session(Uuid::new_v4()).await?;
        assert!(lines.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn append_emits_firehose_event_with_redacted_body() -> anyhow::Result<()> {
        use crate::agent::agent_events::BroadcastAgentEventEmitter;
        use nexo_config::types::transcripts::RedactionConfig;
        use nexo_tool_meta::admin::agent_events::AgentEventKind;

        let root = tmp_dir("emit_redacted");
        let cfg = RedactionConfig {
            enabled: true,
            use_builtins: true,
            extra_patterns: Vec::new(),
        };
        let redactor = Arc::new(Redactor::from_config(&cfg)?);
        let emitter = Arc::new(BroadcastAgentEventEmitter::new());
        let mut rx = emitter.subscribe();
        let writer = TranscriptWriter::with_extras(&root, "kate", redactor, None)
            .with_emitter(emitter.clone());

        let session_id = Uuid::new_v4();
        writer
            .append_entry(
                session_id,
                user_entry("leak sk-abcdefghijklmnopqrstuvwx0123 here", "wa"),
            )
            .await?;
        writer
            .append_entry(session_id, user_entry("second message", "wa"))
            .await?;

        let first = rx.recv().await?;
        let second = rx.recv().await?;
        match (&first, &second) {
            (
                AgentEventKind::TranscriptAppended { seq: 0, body: b1, .. },
                AgentEventKind::TranscriptAppended { seq: 1, body: b2, .. },
            ) => {
                assert!(b1.contains("[REDACTED:"), "first body redacted: {b1}");
                assert!(!b1.contains("sk-abcdef"), "raw secret leaked: {b1}");
                assert_eq!(b2, "second message");
            }
            other => panic!("unexpected events: {other:?}"),
        }
        tokio::fs::remove_dir_all(&root).await.ok();
        Ok(())
    }

    #[tokio::test]
    async fn append_emit_carries_tenant_id_when_writer_tagged() -> anyhow::Result<()> {
        use crate::agent::agent_events::BroadcastAgentEventEmitter;
        use nexo_tool_meta::admin::agent_events::AgentEventKind;

        let root = tmp_dir("emit_tenant_some");
        let emitter = Arc::new(BroadcastAgentEventEmitter::new());
        let mut rx = emitter.subscribe();
        let writer = TranscriptWriter::new(&root, "kate")
            .with_tenant_id(Some("acme".into()))
            .with_emitter(emitter.clone());

        let session_id = Uuid::new_v4();
        writer
            .append_entry(session_id, user_entry("hi", "wa"))
            .await?;

        let event = rx.recv().await?;
        match event {
            AgentEventKind::TranscriptAppended { tenant_id, .. } => {
                assert_eq!(tenant_id.as_deref(), Some("acme"));
            }
            other => panic!("unexpected event kind: {other:?}"),
        }
        tokio::fs::remove_dir_all(&root).await.ok();
        Ok(())
    }

    #[tokio::test]
    async fn append_emit_legacy_writer_carries_none_tenant_id() -> anyhow::Result<()> {
        use crate::agent::agent_events::BroadcastAgentEventEmitter;
        use nexo_tool_meta::admin::agent_events::AgentEventKind;

        let root = tmp_dir("emit_tenant_none");
        let emitter = Arc::new(BroadcastAgentEventEmitter::new());
        let mut rx = emitter.subscribe();
        // No `with_tenant_id` — legacy single-tenant path.
        let writer = TranscriptWriter::new(&root, "kate").with_emitter(emitter.clone());

        let session_id = Uuid::new_v4();
        writer
            .append_entry(session_id, user_entry("hi", "wa"))
            .await?;

        let event = rx.recv().await?;
        match event {
            AgentEventKind::TranscriptAppended { tenant_id, .. } => {
                assert_eq!(tenant_id, None, "legacy writer must not stamp tenant_id");
            }
            other => panic!("unexpected event kind: {other:?}"),
        }
        tokio::fs::remove_dir_all(&root).await.ok();
        Ok(())
    }

    #[tokio::test]
    async fn with_extras_redactor_redacts_jsonl() -> anyhow::Result<()> {
        use nexo_config::types::transcripts::RedactionConfig;
        let root = tmp_dir("redact");
        let cfg = RedactionConfig {
            enabled: true,
            use_builtins: true,
            extra_patterns: Vec::new(),
        };
        let redactor = Arc::new(Redactor::from_config(&cfg)?);
        let writer = TranscriptWriter::with_extras(&root, "kate", redactor, None);
        let session_id = Uuid::new_v4();
        writer
            .append_entry(
                session_id,
                user_entry("token sk-abcdefghijklmnopqrstuvwx0123 ok", "wa"),
            )
            .await?;
        let lines = writer.read_session(session_id).await?;
        let entry = match &lines[1] {
            TranscriptLine::Entry(e) => e,
            _ => panic!(),
        };
        assert!(entry.content.contains("[REDACTED:openai_key]"));
        assert!(!entry.content.contains("sk-abcdef"));
        tokio::fs::remove_dir_all(&root).await.ok();
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_first_appends_only_write_one_header() -> anyhow::Result<()> {
        let root = tmp_dir("race");
        let session_id = Uuid::new_v4();
        let mut handles = Vec::new();
        for i in 0..16 {
            let r = root.clone();
            handles.push(tokio::spawn(async move {
                let writer = TranscriptWriter::new(r, "kate");
                writer
                    .append_entry(session_id, user_entry(&format!("msg-{i}"), "wa"))
                    .await
            }));
        }
        for h in handles {
            h.await.unwrap()?;
        }
        let writer = TranscriptWriter::new(&root, "kate");
        let lines = writer.read_session(session_id).await?;
        let header_count = lines
            .iter()
            .filter(|l| matches!(l, TranscriptLine::Session(_)))
            .count();
        let entry_count = lines
            .iter()
            .filter(|l| matches!(l, TranscriptLine::Entry(_)))
            .count();
        assert_eq!(header_count, 1, "exactly one header expected");
        assert_eq!(entry_count, 16, "all 16 entries persisted");
        tokio::fs::remove_dir_all(&root).await.ok();
        Ok(())
    }

    #[tokio::test]
    async fn with_extras_index_inserts_row() -> anyhow::Result<()> {
        let root = tmp_dir("idxwire");
        let idx_path = root.join("transcripts.db");
        let idx = Arc::new(TranscriptsIndex::open(&idx_path).await?);
        let writer = TranscriptWriter::with_extras(
            &root,
            "kate",
            Arc::new(Redactor::disabled()),
            Some(idx.clone()),
        );
        let session_id = Uuid::new_v4();
        writer
            .append_entry(session_id, user_entry("buscame esto", "wa"))
            .await?;
        let n = idx.count_for_agent("kate").await?;
        assert_eq!(n, 1);
        let hits = idx.search("kate", "buscame", 10).await?;
        assert_eq!(hits.len(), 1);
        tokio::fs::remove_dir_all(&root).await.ok();
        Ok(())
    }
}
