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
use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
pub const TRANSCRIPT_VERSION: u32 = 1;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptLine {
    Session(SessionHeader),
    Entry(TranscriptEntry),
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
}
impl TranscriptWriter {
    pub fn new(root: impl Into<PathBuf>, agent_id: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            agent_id: agent_id.into(),
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn session_path(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl"))
    }
    /// Append one entry to the session transcript. Writes a header first if
    /// the file doesn't exist yet.
    pub async fn append_entry(
        &self,
        session_id: Uuid,
        entry: TranscriptEntry,
    ) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(&self.root).await?;
        let path = self.session_path(session_id);
        let exists = tokio::fs::try_exists(&path).await.unwrap_or(false);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        if !exists {
            let header = TranscriptLine::Session(SessionHeader {
                version: TRANSCRIPT_VERSION,
                id: session_id,
                timestamp: Utc::now(),
                agent_id: self.agent_id.clone(),
                source_plugin: entry.source_plugin.clone(),
            });
            write_jsonl(&mut file, &header).await?;
        }
        write_jsonl(&mut file, &TranscriptLine::Entry(entry)).await?;
        file.flush().await?;
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
}
