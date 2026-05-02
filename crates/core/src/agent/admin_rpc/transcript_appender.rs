//! Phase 82.13.b.1 — `TranscriptAppender` trait.
//!
//! Decouples the admin-RPC layer (handler `processing/intervention`
//! and `processing/resume`) from the concrete `TranscriptWriter`
//! type. The handler doesn't import
//! `nexo_core::agent::transcripts::TranscriptWriter` directly —
//! that would force every admin-RPC unit test to spin up the real
//! writer with its filesystem + redactor + FTS deps. Instead the
//! handler depends on this trait; the production adapter lives in
//! `nexo-setup` and wraps the writer.
//!
//! v0 only stamps three flavours of entry:
//!
//! 1. **Operator reply** (Phase 82.13.b.1) —
//!    `role: Assistant`, `source_plugin: "intervention:<channel>"`,
//!    `sender_id: "operator:<token_hash>"`. Lets the agent treat
//!    the operator's manual reply as if it had said it on its
//!    next turn (continuity of tone, no system-prompt
//!    engineering).
//! 2. **Operator summary** (Phase 82.13.b.2) — `role: System`,
//!    content prefixed with `[operator_summary] `, same
//!    `source_plugin` discriminator family but with a `:summary`
//!    suffix.
//! 3. **Replayed inbound** (Phase 82.13.b.3) — `role: User`,
//!    timestamp preserved from when the inbound originally
//!    arrived during the pause.
//!
//! Every variant is "best effort" — the channel send always
//! happens first; transcript stamping is a side effect that
//! degrades gracefully (`transcript_stamped: Some(false)` on the
//! ack) when the appender is missing, the session id is missing,
//! or persistence fails.

use async_trait::async_trait;
use uuid::Uuid;

/// Speaker role mirror — kept here so the admin-RPC crate does
/// not pull in `nexo_core::agent::transcripts::TranscriptRole`.
/// The setup-side adapter translates this into the writer's
/// own enum at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptRole {
    /// Inbound from end-user.
    User,
    /// Outbound from the agent (or, with the right
    /// `sender_id`/`source_plugin`, from an operator standing
    /// in for the agent).
    Assistant,
    /// Tool call result echoed back into the transcript.
    Tool,
    /// System-level event (system prompt, operator summary,
    /// control message).
    System,
}

/// Mirror of `nexo_core::agent::transcripts::TranscriptEntry` —
/// kept here to keep the admin-RPC layer decoupled from the
/// writer module. The setup-side adapter maps this into the
/// writer's own struct.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    /// Speaker.
    pub role: TranscriptRole,
    /// Body — already trimmed by the caller. The writer
    /// applies the redactor on top before persistence so PII
    /// in operator-typed bodies still gets redacted on disk.
    pub content: String,
    /// Discriminator the firehose / FTS index / UI use to tell
    /// agent-LLM output (`<channel>`) apart from operator
    /// stand-in (`intervention:<channel>`) apart from operator
    /// summary (`intervention:summary`).
    pub source_plugin: String,
    /// Opaque sender id. v0 conventions:
    /// - `Some("operator:<token_hash>")` for operator stand-in.
    /// - `Some(<contact_id>)` for replayed inbounds.
    /// - `None` for agent-LLM output.
    pub sender_id: Option<String>,
    /// Channel-side message id when applicable (e.g. WAID for
    /// WhatsApp). Threaded through so future audit + reply-to
    /// correlation works.
    pub message_id: Option<Uuid>,
}

/// Hook the admin-RPC handler calls to stamp one entry against
/// `(agent_id, session_id)`. Implementations MUST be best-effort
/// and SHOULD log failures rather than panic — the operator's
/// channel send already succeeded by the time `append` is called,
/// so a persistence error must not surface as a failed RPC.
#[async_trait]
pub trait TranscriptAppender: Send + Sync + std::fmt::Debug {
    /// Append `entry` under the given `(agent_id, session_id)`.
    /// Returns `Ok(())` on a successful persistence; the handler
    /// translates `Err(_)` into `transcript_stamped: Some(false)`
    /// on the operator-facing ack.
    async fn append(
        &self,
        agent_id: &str,
        session_id: Uuid,
        entry: TranscriptEntry,
    ) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Recording stub used by handler tests in
    /// `domains::processing` — captures `(agent_id, session_id,
    /// entry)` triples without persistence.
    #[derive(Debug, Default, Clone)]
    pub struct RecordingTranscriptAppender {
        pub captured: Arc<Mutex<Vec<(String, Uuid, TranscriptEntry)>>>,
        /// When `Some`, `append` returns this error instead of
        /// recording — used to verify graceful degradation.
        pub fail_with: Option<String>,
    }

    impl RecordingTranscriptAppender {
        fn new_failing(msg: impl Into<String>) -> Self {
            Self {
                captured: Arc::new(Mutex::new(Vec::new())),
                fail_with: Some(msg.into()),
            }
        }

        fn calls(&self) -> Vec<(String, Uuid, TranscriptEntry)> {
            self.captured.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl TranscriptAppender for RecordingTranscriptAppender {
        async fn append(
            &self,
            agent_id: &str,
            session_id: Uuid,
            entry: TranscriptEntry,
        ) -> anyhow::Result<()> {
            if let Some(msg) = &self.fail_with {
                return Err(anyhow::anyhow!(msg.clone()));
            }
            self.captured
                .lock()
                .unwrap()
                .push((agent_id.into(), session_id, entry));
            Ok(())
        }
    }

    /// Trait must be object-safe so the dispatcher can hold an
    /// `Option<Arc<dyn TranscriptAppender>>`.
    #[test]
    fn trait_is_object_safe() {
        fn _accepts(_: &dyn TranscriptAppender) {}
    }

    #[tokio::test]
    async fn recording_appender_captures_entries() {
        let app = RecordingTranscriptAppender::default();
        let session = Uuid::new_v4();
        let entry = TranscriptEntry {
            role: TranscriptRole::Assistant,
            content: "hola".into(),
            source_plugin: "intervention:whatsapp".into(),
            sender_id: Some("operator:abc".into()),
            message_id: None,
        };
        app.append("ana", session, entry.clone()).await.unwrap();
        let calls = app.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "ana");
        assert_eq!(calls[0].1, session);
        assert_eq!(calls[0].2.content, "hola");
        assert_eq!(calls[0].2.source_plugin, "intervention:whatsapp");
        assert!(matches!(calls[0].2.role, TranscriptRole::Assistant));
    }

    #[tokio::test]
    async fn failing_appender_returns_error_without_recording() {
        let app = RecordingTranscriptAppender::new_failing("disk full");
        let entry = TranscriptEntry {
            role: TranscriptRole::System,
            content: "x".into(),
            source_plugin: "intervention:summary".into(),
            sender_id: None,
            message_id: None,
        };
        let r = app.append("ana", Uuid::new_v4(), entry).await;
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().to_string(), "disk full");
        assert!(app.calls().is_empty());
    }

    /// Role enum is `Copy` so handlers can pattern-match without
    /// borrowing.
    #[test]
    fn role_enum_is_copy() {
        let r: TranscriptRole = TranscriptRole::User;
        let _r2 = r;
        let _r3 = r;
    }
}
