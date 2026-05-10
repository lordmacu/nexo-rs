//! Phase 91.x.wasm.phase-4 — cloud STT backends.
//!
//! Provides a uniform [`SttProvider`] trait that wraps three
//! concrete impls:
//!
//! - [`openai::OpenAiProvider`] — Whisper-1 via the public
//!   `/v1/audio/transcriptions` REST endpoint.
//! - [`groq::GroqProvider`] — Whisper-large-v3 via the
//!   OpenAI-compatible `openai/v1/audio/transcriptions`
//!   endpoint (Groq exposes it under their domain).
//! - [`anthropic::AnthropicVoiceStream`] — the
//!   `voice_stream` WebSocket per the `claude-code-leak`
//!   mining; OAuth-gated. **Not yet implemented** — placeholder
//!   for Phase 91.x.wasm.phase-4b.
//!
//! Plus a [`CompositeProvider`] fallback chain so callers can
//! configure cloud-first with Candle as a safety net (or any
//! provider order). The fallback fires on transport failures —
//! HTTP 5xx, network unreachable, WebSocket disconnect — so
//! transient cloud outages don't drop voice notes.
//!
//! Cross-platform: every provider uses `reqwest` (browser fetch
//! API on `wasm32-unknown-unknown`, native HTTP elsewhere).
//! Works inside a WASM SDK build that has `stt-cloud` enabled
//! and `stt-candle` (the local backend) gated out.

#![cfg(feature = "stt-cloud")]
// Phase 91.x.wasm.phase-4 module — allow brief builder-method
// docs to live in the trait + outer-section comments rather
// than per-fn. The crate-level `deny(missing_docs)` would
// otherwise require Doc comments on every `with_*` builder
// which adds noise without information density.
#![allow(missing_docs)]

use std::fmt;

use async_trait::async_trait;
use serde::Deserialize;

use super::SttError;

pub mod anthropic;
pub mod groq;
pub mod openai;

// Phase 91.x.wasm.phase-4d — bridge that wraps the local Candle
// backend behind the cloud `SttProvider` trait so it can sit at
// the tail of a `CompositeProvider` chain. Only compiled when
// `stt-cloud` AND `stt-candle` are both on; cloud-only WASM
// builds skip it (Candle inference path can't run on wasm32
// today — Phase 91.x.wasm.phase-4 follow-up).
#[cfg(feature = "stt-candle")]
pub mod local_candle;

/// What every cloud STT provider must implement: take a raw
/// audio buffer (any format the upstream understands —
/// ogg-opus, mp3, wav, m4a, ...), an optional language hint,
/// and return the transcript text.
///
/// `audio_mime` is the canonical MIME of the input bytes
/// (`audio/ogg`, `audio/mpeg`, ...). Some providers ignore it
/// and rely on the filename extension; we pass it through
/// either way so the multipart upload is correctly tagged.
#[async_trait]
pub trait SttProvider: Send + Sync + fmt::Debug {
    async fn transcribe(
        &self,
        audio_bytes: Vec<u8>,
        audio_mime: &str,
        lang_hint: Option<&str>,
    ) -> Result<String, SttError>;

    /// Display-friendly name for the provider, used in logs +
    /// the `CompositeProvider` fallback chain when reporting
    /// which leg actually produced the transcript.
    fn name(&self) -> &'static str;
}

/// Decoded body shape every OpenAI-compatible endpoint returns.
/// Groq + OpenAI + (future) Anthropic-REST share this — the
/// service contracts converged on the JSON `{"text": "..."}`
/// shape.
#[derive(Deserialize)]
struct OpenAiCompatibleTranscription {
    text: String,
}

/// Shared HTTP POST + multipart helper for the OpenAI / Groq
/// REST shape. The two providers differ only in endpoint URL
/// + model id + (optionally) the Bearer token; the wire is
/// otherwise identical, so the actual HTTP work lives here
/// once.
async fn post_openai_compatible(
    endpoint: &str,
    api_key: &str,
    model: &str,
    audio_bytes: Vec<u8>,
    audio_mime: &str,
    lang_hint: Option<&str>,
) -> Result<String, SttError> {
    let part = reqwest::multipart::Part::bytes(audio_bytes)
        .file_name("audio")
        .mime_str(audio_mime)
        .map_err(|e| SttError::Decode(format!("invalid audio MIME {audio_mime:?}: {e}")))?;

    let mut form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .text("response_format", "json".to_string())
        .part("file", part);

    if let Some(lang) = lang_hint.filter(|l| !l.is_empty() && *l != "auto") {
        // Whisper REST accepts BCP-47 language hints; split off
        // any region subtag because the provider only honours
        // the language-level code (`es-AR` → `es`).
        let base = lang.split(|c| c == '-' || c == '_').next().unwrap_or(lang);
        form = form.text("language", base.to_lowercase());
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(endpoint)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| SttError::Whisper(format!("cloud HTTP send: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(SttError::Whisper(format!(
            "cloud STT HTTP {status}: {body}"
        )));
    }

    let parsed: OpenAiCompatibleTranscription = resp
        .json()
        .await
        .map_err(|e| SttError::Whisper(format!("cloud JSON parse: {e}")))?;
    let text = parsed.text.trim().to_string();
    if text.is_empty() {
        return Err(SttError::EmptyTranscript);
    }
    Ok(text)
}

/// Chain two or more providers and try them in order. Fallback
/// triggers on `SttError::Whisper(...)` (transport / API failures)
/// — explicit operator errors like `EmptyAudio` or
/// `UnsupportedFormat` are surfaced directly because the same
/// failure would repeat on every leg.
///
/// Typical usage: cloud as the primary, Candle local as the
/// fallback for offline / quota-exhausted scenarios. The chain
/// is order-sensitive (left to right). Empty chain returns an
/// error on every call.
#[derive(Debug)]
pub struct CompositeProvider {
    providers: Vec<Box<dyn SttProvider>>,
}

impl CompositeProvider {
    pub fn new(providers: Vec<Box<dyn SttProvider>>) -> Self {
        Self { providers }
    }

    pub fn push(&mut self, provider: Box<dyn SttProvider>) {
        self.providers.push(provider);
    }
}

#[async_trait]
impl SttProvider for CompositeProvider {
    async fn transcribe(
        &self,
        audio_bytes: Vec<u8>,
        audio_mime: &str,
        lang_hint: Option<&str>,
    ) -> Result<String, SttError> {
        if self.providers.is_empty() {
            return Err(SttError::Whisper(
                "CompositeProvider has no legs configured — set at least one provider".into(),
            ));
        }
        let mut last_err: Option<SttError> = None;
        for (idx, provider) in self.providers.iter().enumerate() {
            // Clone bytes because every provider takes ownership;
            // we can't share a reference across an async trait
            // call that has its own Send bound.
            let bytes = audio_bytes.clone();
            match provider.transcribe(bytes, audio_mime, lang_hint).await {
                Ok(text) => {
                    tracing::info!(
                        target: "stt.cloud",
                        provider = provider.name(),
                        leg = idx,
                        transcript_len = text.len(),
                        "composite STT transcription ok"
                    );
                    return Ok(text);
                }
                Err(err) => {
                    // Hard-stop on non-transport failures —
                    // the next leg would hit the same audio
                    // problem.
                    if matches!(
                        &err,
                        SttError::EmptyAudio
                            | SttError::UnsupportedFormat(_)
                            | SttError::Decode(_)
                    ) {
                        return Err(err);
                    }
                    tracing::warn!(
                        target: "stt.cloud",
                        provider = provider.name(),
                        leg = idx,
                        error = %err,
                        "composite STT leg failed, trying next"
                    );
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            SttError::Whisper("CompositeProvider exhausted every leg without success".into())
        }))
    }

    fn name(&self) -> &'static str {
        "composite"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test stub — returns the configured response.
    struct StubProvider {
        outcome: Box<dyn Fn() -> Result<String, SttError> + Send + Sync>,
        name: &'static str,
    }

    impl fmt::Debug for StubProvider {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            // `Box<dyn Fn>` isn't `Debug`; print the name + a
            // marker so test failures still carry useful context.
            f.debug_struct("StubProvider")
                .field("name", &self.name)
                .field("outcome", &"<dyn Fn>")
                .finish()
        }
    }

    #[async_trait]
    impl SttProvider for StubProvider {
        async fn transcribe(
            &self,
            _audio: Vec<u8>,
            _mime: &str,
            _lang: Option<&str>,
        ) -> Result<String, SttError> {
            (self.outcome)()
        }
        fn name(&self) -> &'static str {
            self.name
        }
    }

    fn ok_leg(text: &'static str) -> Box<dyn SttProvider> {
        Box::new(StubProvider {
            outcome: Box::new(move || Ok(text.to_string())),
            name: text,
        })
    }

    fn whisper_err_leg(name: &'static str) -> Box<dyn SttProvider> {
        Box::new(StubProvider {
            outcome: Box::new(|| Err(SttError::Whisper("transport blow-up".into()))),
            name,
        })
    }

    fn decode_err_leg(name: &'static str) -> Box<dyn SttProvider> {
        Box::new(StubProvider {
            outcome: Box::new(|| Err(SttError::Decode("bad audio".into()))),
            name,
        })
    }

    #[tokio::test]
    async fn composite_first_leg_ok_returns_immediately() {
        let chain = CompositeProvider::new(vec![ok_leg("primary"), ok_leg("backup")]);
        let out = chain.transcribe(vec![1, 2, 3], "audio/ogg", None).await.unwrap();
        assert_eq!(out, "primary");
    }

    #[tokio::test]
    async fn composite_transport_failure_falls_through_to_next() {
        let chain = CompositeProvider::new(vec![
            whisper_err_leg("primary"),
            ok_leg("backup"),
        ]);
        let out = chain.transcribe(vec![1, 2, 3], "audio/ogg", None).await.unwrap();
        assert_eq!(out, "backup");
    }

    #[tokio::test]
    async fn composite_decode_failure_short_circuits() {
        // Decode failures are not transport — the next leg would
        // see the same broken audio. Must NOT fall through.
        let chain = CompositeProvider::new(vec![
            decode_err_leg("primary"),
            ok_leg("backup"),
        ]);
        let err = match chain.transcribe(vec![1, 2, 3], "audio/ogg", None).await {
            Ok(t) => panic!("expected error, got {t:?}"),
            Err(e) => e,
        };
        assert!(matches!(err, SttError::Decode(_)));
    }

    #[tokio::test]
    async fn composite_empty_chain_errors() {
        let chain = CompositeProvider::new(vec![]);
        let err = match chain.transcribe(vec![], "audio/ogg", None).await {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(matches!(err, SttError::Whisper(_)));
    }

    #[tokio::test]
    async fn composite_all_legs_fail_returns_last_error() {
        let chain = CompositeProvider::new(vec![
            whisper_err_leg("primary"),
            whisper_err_leg("backup"),
        ]);
        let err = match chain.transcribe(vec![1], "audio/ogg", None).await {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(matches!(err, SttError::Whisper(_)));
    }
}
