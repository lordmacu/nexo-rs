//! `audio_stt_inbound_transform` tool handler — decodes the
//! framework's auto-discovered `*_inbound_transform` wire shape,
//! delegates to [`super::transcribe::transcribe_file`], and
//! returns the transcript (or a passthrough marker on failure).
//!
//! Wire shape (tool args):
//! ```json
//! {
//!   "context": { agent_id, session_id, channel, instance,
//!                sender_id, tenant_id, conversation_key },
//!   "text":    "<current text — empty for voice notes>",
//!   "media":   { "kind": "audio_voice"|"audio", "path": "...",
//!                "mime_type": "audio/ogg; codecs=opus" } | null
//! }
//! ```
//! Wire shape (return):
//! - `{ ok: true, text: "<transcript>" }` — agent receives the
//!   transcript verbatim.
//! - `{ ok: true, passthrough: true }` — non-audio inbound, skip.
//! - `{ ok: true, passthrough: true, fallback_reason: "<msg>" }`
//!   — STT failed; framework keeps the original (likely empty)
//!   text + the operator gets a warn log.
//! - `{ ok: false, error: "<msg>" }` — surfaced when the audio
//!   file is missing on disk (the only case where we want the
//!   framework to know something went wrong without dropping the
//!   turn).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::TranscribeConfig;
use crate::ctx::ToolCtx;
use crate::errors::ToolError;
use crate::reply::ToolReply;
use crate::runtime::ToolHandler;

#[derive(Debug, Deserialize)]
struct InboundMedia {
    #[serde(default)]
    kind: String,
    path: String,
    #[serde(default)]
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InboundTransformArgs {
    #[serde(default)]
    text: String,
    #[serde(default)]
    media: Option<InboundMedia>,
}

/// `ToolHandler` for the framework's `*_inbound_transform`
/// pipeline. Hand to
/// `Microapp::with_tool("audio_stt_inbound_transform", handler)`.
///
/// The handler owns an `Arc<TranscribeConfig>` so multiple
/// in-flight invocations share one config without re-reading env.
pub struct InboundTransformHandler {
    cfg: Arc<TranscribeConfig>,
}

impl InboundTransformHandler {
    /// Build a new handler. Wraps the supplied `TranscribeConfig`
    /// in an `Arc` so callers may stash one upstream copy.
    pub fn new(cfg: Arc<TranscribeConfig>) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl ToolHandler for InboundTransformHandler {
    async fn call(&self, args: Value, _ctx: ToolCtx) -> Result<ToolReply, ToolError> {
        let parsed: InboundTransformArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(format!("inbound transform args: {e}")))?;

        // Entry-log so operators can see whether the framework's
        // `*_inbound_transform` discovery actually invoked us. The
        // hot path stays cheap — one `tracing::info!` per inbound,
        // gated on the subscriber level.
        let media_kind_log = parsed.media.as_ref().map(|m| m.kind.clone());
        let media_path_log = parsed.media.as_ref().map(|m| m.path.clone());
        let media_mime_log = parsed.media.as_ref().and_then(|m| m.mime_type.clone());
        tracing::info!(
            text_len = parsed.text.len(),
            media_kind = ?media_kind_log,
            media_path = ?media_path_log,
            media_mime = ?media_mime_log,
            "stt: inbound transform invoked"
        );

        let media = match parsed.media {
            Some(m) => m,
            None => {
                tracing::debug!(
                    "stt: passthrough (no media on inbound)"
                );
                return Ok(ToolReply::ok_json(
                    json!({ "ok": true, "passthrough": true }),
                ));
            }
        };
        let is_audio = media.kind == "audio_voice"
            || media.kind == "audio"
            || media
                .mime_type
                .as_deref()
                .map(|m| m.starts_with("audio/"))
                .unwrap_or(false);
        if !is_audio {
            tracing::debug!(
                kind = %media.kind,
                mime = ?media.mime_type,
                "stt: passthrough (non-audio media)"
            );
            return Ok(ToolReply::ok_json(
                json!({ "ok": true, "passthrough": true }),
            ));
        }
        let path = PathBuf::from(&media.path);
        if !path.is_file() {
            let msg = format!("audio file missing on disk: {}", path.display());
            tracing::warn!(path = %path.display(), "stt: audio file not found");
            return Ok(ToolReply::ok_json(json!({
                "ok": false,
                "error": msg,
            })));
        }
        tracing::info!(
            path = %path.display(),
            mime = ?media.mime_type,
            original_text_len = parsed.text.len(),
            "stt: transcribing voice note",
        );
        match super::transcribe_file(&path, &self.cfg).await {
            Ok(transcript) => {
                // Surface the full transcript at INFO so operators
                // can verify the STT pipeline against what the LLM
                // actually received. Truncate to keep the log line
                // readable; bigger payloads still go to the agent
                // verbatim.
                let preview: String = transcript.chars().take(400).collect();
                tracing::info!(
                    path = %path.display(),
                    transcript_len = transcript.len(),
                    transcript = %preview,
                    "stt: transcribed audio",
                );
                Ok(ToolReply::ok_json(json!({
                    "ok": true,
                    "text": transcript,
                })))
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "stt: transcription failed; passing through original text",
                );
                // Passthrough on failure rather than dropping the
                // turn — the agent will see whatever (probably
                // empty) text was there before. Operator gets a
                // warn log to diagnose.
                Ok(ToolReply::ok_json(json!({
                    "ok": true,
                    "passthrough": true,
                    "fallback_reason": format!("stt_failed: {e}"),
                })))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::ToolCtx;

    fn handler() -> InboundTransformHandler {
        InboundTransformHandler::new(Arc::new(TranscribeConfig::default()))
    }

    fn empty_ctx() -> ToolCtx {
        // Minimal ctx the wire-shape branches don't read from.
        // Field set mirrors `ctx::tests::ctx_with_binding` so the
        // feature-gated `outbound`/`admin` fields are populated
        // when those features are enabled in the test build.
        ToolCtx {
            agent_id: "test".into(),
            session_id: None,
            binding: None,
            inbound: None,
            #[cfg(not(feature = "outbound"))]
            _outbound_marker: std::marker::PhantomData,
            #[cfg(feature = "outbound")]
            outbound: Arc::new(crate::outbound::OutboundDispatcher::new_stub()),
            #[cfg(feature = "admin")]
            admin: None,
        }
    }

    #[tokio::test]
    async fn no_media_returns_passthrough() {
        let r = handler()
            .call(json!({ "text": "hi" }), empty_ctx())
            .await
            .unwrap();
        let v: Value = r.into_value();
        assert_eq!(v["ok"], true);
        assert_eq!(v["passthrough"], true);
    }

    #[tokio::test]
    async fn non_audio_media_returns_passthrough() {
        let r = handler()
            .call(
                json!({
                    "text": "",
                    "media": { "kind": "image", "path": "/tmp/x.png", "mime_type": "image/png" }
                }),
                empty_ctx(),
            )
            .await
            .unwrap();
        let v: Value = r.into_value();
        assert_eq!(v["passthrough"], true);
    }

    #[tokio::test]
    async fn missing_audio_file_returns_ok_false_with_error() {
        let r = handler()
            .call(
                json!({
                    "text": "",
                    "media": {
                        "kind": "audio_voice",
                        "path": "/nonexistent/voice.ogg",
                        "mime_type": "audio/ogg"
                    }
                }),
                empty_ctx(),
            )
            .await
            .unwrap();
        let v: Value = r.into_value();
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("missing on disk"));
    }

    #[tokio::test]
    async fn invalid_args_surface_as_invalid_arguments() {
        // `media` not an object — serde rejects.
        let r = handler()
            .call(json!({ "media": "not-an-object" }), empty_ctx())
            .await;
        assert!(matches!(r, Err(ToolError::InvalidArguments(_))));
    }
}
