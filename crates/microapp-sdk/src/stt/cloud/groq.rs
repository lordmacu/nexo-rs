//! Phase 91.x.wasm.phase-4 — Groq Whisper REST provider.
//!
//! Groq hosts an OpenAI-compatible audio transcription endpoint
//! at `POST https://api.groq.com/openai/v1/audio/transcriptions`.
//! Wire shape is identical to OpenAI's (multipart upload,
//! Bearer auth, `{"text": "..."}` JSON reply) so the actual
//! HTTP work lives in the shared `post_openai_compatible`
//! helper next door.
//!
//! What Groq adds vs OpenAI:
//!
//! - **Whisper-large-v3** as the default model (OpenAI only
//!   exposes `whisper-1` which is the older v2 family).
//! - **2–5× lower latency** thanks to their custom Whisper
//!   inference stack on LPU hardware.
//! - **Cheaper per-minute pricing** ($0.04/h vs $0.36/h on
//!   OpenAI as of 2026).
//!
//! Auth: Bearer token from a Groq Cloud Console API key.

#![cfg(feature = "stt-cloud")]

use async_trait::async_trait;

use super::{post_openai_compatible, SttProvider};
use crate::stt::SttError;

pub const DEFAULT_ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
pub const DEFAULT_MODEL: &str = "whisper-large-v3";

#[derive(Debug, Clone)]
pub struct GroqProvider {
    endpoint: String,
    api_key: String,
    model: String,
}

impl GroqProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_string(),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Pick a different Groq Whisper variant. Today the options
    /// are `whisper-large-v3` (default) and
    /// `whisper-large-v3-turbo` (faster, slightly higher WER).
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

#[async_trait]
impl SttProvider for GroqProvider {
    async fn transcribe(
        &self,
        audio_bytes: Vec<u8>,
        audio_mime: &str,
        lang_hint: Option<&str>,
    ) -> Result<String, SttError> {
        post_openai_compatible(
            &self.endpoint,
            &self.api_key,
            &self.model,
            audio_bytes,
            audio_mime,
            lang_hint,
        )
        .await
    }

    fn name(&self) -> &'static str {
        "groq"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_at_groq_dot_com() {
        let p = GroqProvider::new("gsk_...");
        assert_eq!(p.endpoint, DEFAULT_ENDPOINT);
        assert_eq!(p.model, DEFAULT_MODEL);
    }

    #[test]
    fn builder_overrides_endpoint_and_model() {
        let p = GroqProvider::new("gsk_...")
            .with_endpoint("https://groq-mirror.example/v1/audio/transcriptions")
            .with_model("whisper-large-v3-turbo");
        assert_eq!(
            p.endpoint,
            "https://groq-mirror.example/v1/audio/transcriptions"
        );
        assert_eq!(p.model, "whisper-large-v3-turbo");
    }
}
