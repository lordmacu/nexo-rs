//! Phase 91.x.wasm.phase-4b — Anthropic `voice_stream` STT
//! provider (placeholder).
//!
//! Wire reference: `claude-code-leak/src/services/voiceStreamSTT.ts`
//! captures the live protocol the official Claude Code client
//! speaks to Anthropic's internal STT endpoint:
//!
//! - URL: `wss://api.anthropic.com/api/ws/speech_to_text/voice_stream`
//! - Query params: `encoding=linear16`, `sample_rate=16000`,
//!   `channels=1`, `endpointing_ms=300`, `utterance_end_ms=1000`,
//!   `language=<bcp47>`, optional repeated `keyterms=<term>`,
//!   `use_conversation_engine=true`,
//!   `stt_provider=deepgram-nova3` (gated server-side by the
//!   `tengu_cobalt_frost` GrowthBook flag).
//! - Auth: same OAuth Bearer token Claude Code uses
//!   (Claude.ai subscriber session).
//! - JSON control client → server: `{"type":"KeepAlive"}` every
//!   8 s, `{"type":"CloseStream"}` on finalize.
//! - Binary frames client → server: PCM s16 LE @ 16 kHz mono.
//! - JSON server → client: `TranscriptText` (interim, cumulative
//!   on Nova 3), `TranscriptEndpoint` (utterance boundary marker),
//!   `TranscriptError`.
//!
//! Implementation status: **stub**. The full WebSocket client +
//! finalize state machine is substantial (~300 LOC + 4 resolve
//! triggers: TranscriptEndpoint post-CloseStream, no_data_timer,
//! safety_timer, ws_close). Lands separately in Phase
//! 91.x.wasm.phase-4b. Until then, calling `transcribe` on this
//! provider returns `SttError::Whisper("not yet implemented...")`
//! so a `CompositeProvider` chain falls through to the next leg
//! (typically Groq + Candle).

#![cfg(feature = "stt-cloud")]

use async_trait::async_trait;

use super::SttProvider;
use crate::stt::SttError;

/// Anthropic voice_stream STT provider — stub until phase-4b.
///
/// Pre-populated with the OAuth token + optional keyterm list.
/// `with_keyterms` lets the caller boost domain vocabulary
/// (Deepgram Nova 3 feature surfaced through the voice_stream
/// proxy). `with_endpoint` overrides the default Anthropic URL
/// for tests / proxies.
#[derive(Debug, Clone)]
pub struct AnthropicVoiceStream {
    oauth_token: String,
    endpoint: String,
    keyterms: Vec<String>,
}

pub const DEFAULT_ENDPOINT: &str =
    "wss://api.anthropic.com/api/ws/speech_to_text/voice_stream";

impl AnthropicVoiceStream {
    pub fn new(oauth_token: impl Into<String>) -> Self {
        Self {
            oauth_token: oauth_token.into(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            keyterms: Vec::new(),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Domain vocabulary the underlying Deepgram Nova 3 engine
    /// should boost. Surfaces in the `keyterms=<term>` repeated
    /// query param (the voice_stream proxy forwards verbatim to
    /// the STT service).
    pub fn with_keyterms(mut self, keyterms: impl IntoIterator<Item = String>) -> Self {
        self.keyterms = keyterms.into_iter().collect();
        self
    }
}

#[async_trait]
impl SttProvider for AnthropicVoiceStream {
    async fn transcribe(
        &self,
        _audio_bytes: Vec<u8>,
        _audio_mime: &str,
        _lang_hint: Option<&str>,
    ) -> Result<String, SttError> {
        // Defensive guard against accidental calls before the
        // phase-4b WebSocket client lands. Returns a transport-
        // shaped error so `CompositeProvider` falls through to
        // the next leg instead of bubbling it up to the
        // operator.
        let _ = (&self.oauth_token, &self.endpoint, &self.keyterms);
        Err(SttError::Whisper(
            "AnthropicVoiceStream is a placeholder — full WebSocket client lands in \
             Phase 91.x.wasm.phase-4b. Use Groq/OpenAI/Candle in the meantime."
                .into(),
        ))
    }

    fn name(&self) -> &'static str {
        "anthropic-voice-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_at_anthropic_wss() {
        let p = AnthropicVoiceStream::new("sk-ant-oat01-...");
        assert_eq!(p.endpoint, DEFAULT_ENDPOINT);
        assert!(p.keyterms.is_empty());
    }

    #[test]
    fn builder_overrides() {
        let p = AnthropicVoiceStream::new("sk-ant-oat01-...")
            .with_endpoint("wss://localhost:9001/stt")
            .with_keyterms(vec!["nexo".into(), "wa-agent".into()]);
        assert_eq!(p.endpoint, "wss://localhost:9001/stt");
        assert_eq!(p.keyterms, vec!["nexo".to_string(), "wa-agent".to_string()]);
    }

    #[tokio::test]
    async fn transcribe_stub_returns_whisper_error_for_composite_fallthrough() {
        let p = AnthropicVoiceStream::new("sk-ant-oat01-...");
        let err = match p.transcribe(vec![1, 2, 3], "audio/ogg", None).await {
            Ok(t) => panic!("stub must not succeed; got {t:?}"),
            Err(e) => e,
        };
        assert!(matches!(err, SttError::Whisper(_)));
        assert!(err.to_string().contains("phase-4b"), "got: {err}");
    }
}
