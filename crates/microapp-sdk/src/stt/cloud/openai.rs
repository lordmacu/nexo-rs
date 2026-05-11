//! OpenAI Whisper REST provider.
//!
//! Posts a multipart-form-data upload to
//! `POST https://api.openai.com/v1/audio/transcriptions`.
//! Default model is `whisper-1` (only Whisper variant OpenAI
//! hosts as of 2026). The endpoint accepts up to 25 MB of audio
//! in any common format (ogg, mp3, m4a, wav, webm).
//!
//! Auth: Bearer token from the operator's OpenAI account
//! (`OPENAI_API_KEY` env var is the canonical source — but the
//! SDK takes any operator-supplied string so a microapp can
//! source it from its own secrets store).

#![cfg(feature = "stt-cloud-wasm")]

use async_trait::async_trait;

use super::{post_openai_compatible, SttProvider};
use crate::stt::SttError;

/// Default OpenAI Whisper endpoint. Override via
/// `OpenAiProvider::with_endpoint` if you're routing through a
/// proxy or compatible third-party.
pub const DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1/audio/transcriptions";

/// Default Whisper model id on OpenAI. As of 2026 they only
/// expose `whisper-1`; if they ever ship `whisper-large-v3` on
/// the OpenAI domain, swap via `with_model`.
pub const DEFAULT_MODEL: &str = "whisper-1";

#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    endpoint: String,
    api_key: String,
    model: String,
}

impl OpenAiProvider {
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

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl SttProvider for OpenAiProvider {
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
        "openai"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_at_openai_dot_com() {
        let p = OpenAiProvider::new("sk-...");
        assert_eq!(p.endpoint, DEFAULT_ENDPOINT);
        assert_eq!(p.model, DEFAULT_MODEL);
    }

    #[test]
    fn builder_overrides_endpoint_and_model() {
        let p = OpenAiProvider::new("sk-...")
            .with_endpoint("https://proxy.internal/v1/audio/transcriptions")
            .with_model("whisper-large-v3");
        assert_eq!(p.endpoint, "https://proxy.internal/v1/audio/transcriptions");
        assert_eq!(p.model, "whisper-large-v3");
    }
}
