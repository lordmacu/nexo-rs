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

#![cfg(feature = "stt-cloud-wasm")]
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

// Phase 91.x.wasm.phase-4c — `anthropic` carries its own
// `not(target_arch = "wasm32")` cfg gate at the module level
// because tokio-tungstenite (its WebSocket transport) needs
// TCP types absent on wasm32. The REST legs (`openai`, `groq`)
// are wasm-portable after the multipart-body refactor.
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
///
/// Phase 91.x.wasm.phase-4c.3 — on wasm32 we drop the
/// `Send + Sync` bounds + use `async_trait(?Send)` because
/// reqwest's browser fetch backend returns futures holding
/// `Rc<RefCell<...>>` (js-sys types) that can't cross thread
/// boundaries. Wasm32 is single-threaded so the bounds were
/// only there for native multi-threaded runtimes.
#[cfg(not(target_arch = "wasm32"))]
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

/// Phase 91.x.wasm.phase-4c.3 — wasm32 variant of [`SttProvider`]
/// without `Send + Sync` bounds + with `async_trait(?Send)`
/// applied. Required because reqwest's wasm32 backend returns
/// futures holding `Rc<RefCell<...>>` which can't be Send.
/// Single-threaded execution model on wasm32 means the bounds
/// were a no-op anyway.
#[async_trait(?Send)]
#[cfg(target_arch = "wasm32")]
pub trait SttProvider: fmt::Debug {
    async fn transcribe(
        &self,
        audio_bytes: Vec<u8>,
        audio_mime: &str,
        lang_hint: Option<&str>,
    ) -> Result<String, SttError>;

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

/// Shared HTTP POST helper for the OpenAI / Groq REST shape.
/// The two providers differ only in endpoint URL + model id +
/// the Bearer token; the wire is otherwise identical, so the
/// actual HTTP work lives here once.
///
/// Phase 91.x.wasm.phase-4c — multipart body is assembled by
/// hand (raw bytes) instead of via `reqwest::multipart::Form`
/// because the latter is native-only on reqwest; wasm32 exposes
/// only a subset of the builder. Hand-assembled body lets the
/// same code path run on both targets — once the parent module
/// drops the `not(wasm32)` cfg gate, the REST legs work in
/// browser microapps too.
async fn post_openai_compatible(
    endpoint: &str,
    api_key: &str,
    model: &str,
    audio_bytes: Vec<u8>,
    audio_mime: &str,
    lang_hint: Option<&str>,
) -> Result<String, SttError> {
    // RFC 7578 multipart/form-data. Boundary is a string the
    // body never legitimately contains; using a UUID makes
    // collision impossible for any realistic audio buffer.
    let boundary = format!("nexo-stt-{}", uuid::Uuid::new_v4().simple());
    let body = build_openai_multipart_body(
        &boundary,
        model,
        audio_bytes,
        audio_mime,
        lang_hint,
    );
    let content_type = format!("multipart/form-data; boundary={boundary}");

    let client = reqwest::Client::new();
    let resp = client
        .post(endpoint)
        .bearer_auth(api_key)
        .header("content-type", content_type)
        .body(body)
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

/// Phase 91.x.wasm.phase-4c — assemble an RFC 7578
/// `multipart/form-data` body for the OpenAI-compatible
/// `/v1/audio/transcriptions` endpoint. Three fields:
///   - `model` (string)
///   - `response_format` (always `"json"`)
///   - `language` (optional, BCP-47 base — `es-AR` → `es`)
///   - `file` (the raw audio bytes, with `Content-Type` =
///     `audio_mime`, `filename="audio"` per the Whisper-1 +
///     Groq Whisper-large-v3 conventions)
///
/// `boundary` is a caller-chosen string the body never
/// legitimately contains; we use a UUIDv4 simple form upstream
/// for collision-resistance.
///
/// Built into a `Vec<u8>` (not a streaming body) so reqwest's
/// `.body(Vec<u8>)` accepts it on both native and wasm32 —
/// `reqwest::multipart::Form` is native-only, which is what
/// blocked WASM cloud STT pre-phase-4c.
fn build_openai_multipart_body(
    boundary: &str,
    model: &str,
    audio_bytes: Vec<u8>,
    audio_mime: &str,
    lang_hint: Option<&str>,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(audio_bytes.len() + 512);

    // Text field helper. Each field is its own --boundary
    // section terminated by a CRLF before the next boundary.
    let mut push_text = |name: &str, value: &str| {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    };

    push_text("model", model);
    push_text("response_format", "json");

    if let Some(lang) = lang_hint.filter(|l| !l.is_empty() && *l != "auto") {
        let base = lang.split(|c| c == '-' || c == '_').next().unwrap_or(lang);
        push_text("language", &base.to_lowercase());
    }

    // File field — distinct because it carries a filename +
    // Content-Type header before the binary payload.
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio\"\r\n",
    );
    body.extend_from_slice(format!("Content-Type: {audio_mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(&audio_bytes);
    body.extend_from_slice(b"\r\n");

    // Closing boundary — note the trailing `--` (RFC 7578 §4.1)
    // marks end-of-form, not just end-of-part.
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    body
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

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
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

// Phase 91.x.wasm.phase-4d.b — convenience layer on top of
// CompositeProvider + SttProvider.
//
// The user-stated workflow is "try the cloud STT first; if it
// blows up, fall back to local Candle inference." The trait
// surface already supports this — but the boilerplate of
// constructing each provider, wrapping each in a Box,
// assembling the vec, and remembering to add Candle at the
// tail repeats every call site. Three convenience constructors
// + one path-based dispatch helper collapse it to one line.
//
// Cfg-gated so cloud-only consumers (no `stt-candle`) and
// WASM consumers (no `tokio::fs`) skip the additions cleanly.

/// Phase 91.x.wasm.phase-4d.b — read the audio file at `path`
/// into memory and dispatch through an arbitrary
/// [`SttProvider`] chain.
///
/// Convenience wrapper that closes the loop between the
/// path-based public API (`stt::transcribe_file(&Path,
/// &TranscribeConfig)`) and the bytes-based trait surface.
/// MIME is inferred from extension via [`mime_from_path`];
/// unknown extensions fall through as `application/octet-stream`
/// (every cloud provider falls back to byte sniffing in that
/// case).
///
/// Cfg-gated on `not(target_arch = "wasm32")` because it reads
/// from disk via [`tokio::fs::read`]. WASM consumers operate on
/// `Vec<u8>` buffers obtained via browser fetch / File API;
/// they call `chain.transcribe(bytes, mime, lang)` directly.
#[cfg(not(target_arch = "wasm32"))]
pub async fn transcribe_file_with_chain(
    path: &std::path::Path,
    chain: &dyn SttProvider,
    lang_hint: Option<&str>,
) -> Result<String, SttError> {
    let bytes = tokio::fs::read(path).await.map_err(SttError::Io)?;
    if bytes.is_empty() {
        return Err(SttError::EmptyAudio);
    }
    let mime = mime_from_path(path);
    chain.transcribe(bytes, mime, lang_hint).await
}

/// Best-effort MIME lookup from a file extension. Returns the
/// canonical audio MIME every cloud STT provider accepts; falls
/// back to `application/octet-stream` for unknown extensions
/// (provider-side byte sniffing handles that).
#[cfg(not(target_arch = "wasm32"))]
pub fn mime_from_path(path: &std::path::Path) -> &'static str {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        _ => "application/octet-stream",
    }
}

/// Phase 91.x.wasm.phase-4d.b — Anthropic `voice_stream`
/// WebSocket primary + local Candle fallback. One-liner for
/// the user-stated "try cloud, fall back to local" pattern.
///
/// Requires both `stt-cloud-anthropic` (for the WebSocket leg)
/// and `stt-cloud-local-candle` (for the Candle bridge). Pass
/// the same `Arc<TranscribeConfig>` you'd hand to
/// [`crate::stt::InboundTransformHandler::new`] so model
/// auto-fetch + lang hints stay consistent across both legs.
#[cfg(all(feature = "stt-cloud-anthropic", feature = "stt-cloud-local-candle"))]
pub fn anthropic_then_candle(
    anthropic_token: impl Into<String>,
    candle_cfg: std::sync::Arc<crate::stt::TranscribeConfig>,
) -> CompositeProvider {
    CompositeProvider::new(vec![
        Box::new(anthropic::AnthropicVoiceStream::new(anthropic_token)),
        Box::new(local_candle::LocalCandleProvider::new(candle_cfg)),
    ])
}

/// Phase 91.x.wasm.phase-4d.b — OpenAI Whisper-1 REST primary
/// + local Candle fallback. Requires `stt-cloud-local-candle`.
#[cfg(feature = "stt-cloud-local-candle")]
pub fn openai_then_candle(
    api_key: impl Into<String>,
    candle_cfg: std::sync::Arc<crate::stt::TranscribeConfig>,
) -> CompositeProvider {
    CompositeProvider::new(vec![
        Box::new(openai::OpenAiProvider::new(api_key)),
        Box::new(local_candle::LocalCandleProvider::new(candle_cfg)),
    ])
}

/// Phase 91.x.wasm.phase-4d.b — Groq Whisper-large-v3 REST
/// primary + local Candle fallback. Requires
/// `stt-cloud-local-candle`.
#[cfg(feature = "stt-cloud-local-candle")]
pub fn groq_then_candle(
    api_key: impl Into<String>,
    candle_cfg: std::sync::Arc<crate::stt::TranscribeConfig>,
) -> CompositeProvider {
    CompositeProvider::new(vec![
        Box::new(groq::GroqProvider::new(api_key)),
        Box::new(local_candle::LocalCandleProvider::new(candle_cfg)),
    ])
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

    // Phase 91.x.wasm.phase-4d.b tests — MIME picker matrix +
    // path-based dispatch.

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn mime_from_path_matrix() {
        use std::path::Path;
        assert_eq!(mime_from_path(Path::new("voice.ogg")), "audio/ogg");
        assert_eq!(mime_from_path(Path::new("voice.OGG")), "audio/ogg");
        assert_eq!(mime_from_path(Path::new("voice.opus")), "audio/ogg");
        assert_eq!(mime_from_path(Path::new("voice.oga")), "audio/ogg");
        assert_eq!(mime_from_path(Path::new("voice.wav")), "audio/wav");
        assert_eq!(mime_from_path(Path::new("voice.mp3")), "audio/mpeg");
        assert_eq!(mime_from_path(Path::new("voice.m4a")), "audio/mp4");
        assert_eq!(mime_from_path(Path::new("voice.mp4")), "audio/mp4");
        assert_eq!(mime_from_path(Path::new("voice.aac")), "audio/mp4");
        assert_eq!(mime_from_path(Path::new("voice.flac")), "audio/flac");
        assert_eq!(mime_from_path(Path::new("voice.webm")), "audio/webm");
        assert_eq!(
            mime_from_path(Path::new("voice.unknown")),
            "application/octet-stream"
        );
        assert_eq!(mime_from_path(Path::new("noext")), "application/octet-stream");
        // Multi-dot path → extension is "ogg" only.
        assert_eq!(
            mime_from_path(Path::new("/tmp/a.b.c.ogg")),
            "audio/ogg"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn transcribe_file_with_chain_reads_bytes_and_dispatches() {
        use std::sync::{Arc, Mutex};
        // Provider that captures what it was called with.
        struct CapturingProvider {
            captured: Arc<Mutex<Option<(Vec<u8>, String, Option<String>)>>>,
        }
        impl fmt::Debug for CapturingProvider {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "CapturingProvider")
            }
        }
        #[async_trait]
        impl SttProvider for CapturingProvider {
            async fn transcribe(
                &self,
                audio: Vec<u8>,
                mime: &str,
                lang: Option<&str>,
            ) -> Result<String, SttError> {
                *self.captured.lock().unwrap() = Some((
                    audio,
                    mime.to_string(),
                    lang.map(str::to_string),
                ));
                Ok("captured".into())
            }
            fn name(&self) -> &'static str {
                "capturing"
            }
        }

        let tmp = tempfile::Builder::new()
            .prefix("stt-chain-test-")
            .suffix(".ogg")
            .tempfile()
            .unwrap();
        let payload = b"OggS\0\xdeadbeef".to_vec();
        tokio::fs::write(tmp.path(), &payload).await.unwrap();

        let captured = Arc::new(Mutex::new(None));
        let provider = CapturingProvider {
            captured: captured.clone(),
        };
        let result = transcribe_file_with_chain(tmp.path(), &provider, Some("es"))
            .await
            .unwrap();
        assert_eq!(result, "captured");

        let captured = captured.lock().unwrap().clone().unwrap();
        assert_eq!(captured.0, payload);
        assert_eq!(captured.1, "audio/ogg");
        assert_eq!(captured.2.as_deref(), Some("es"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn transcribe_file_with_chain_rejects_empty_file() {
        let tmp = tempfile::Builder::new()
            .prefix("stt-empty-")
            .suffix(".ogg")
            .tempfile()
            .unwrap();
        // Write nothing — file is 0 bytes.
        let chain = CompositeProvider::new(vec![ok_leg("never-called")]);
        let err = transcribe_file_with_chain(tmp.path(), &chain, None)
            .await
            .expect_err("empty file should error");
        assert!(matches!(err, SttError::EmptyAudio));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn transcribe_file_with_chain_io_error_on_missing_path() {
        use std::path::PathBuf;
        let nonexistent = PathBuf::from("/nonexistent/path/voice.ogg");
        let chain = CompositeProvider::new(vec![ok_leg("never-called")]);
        let err = transcribe_file_with_chain(&nonexistent, &chain, None)
            .await
            .expect_err("missing file should error");
        assert!(matches!(err, SttError::Io(_)));
    }

    // Phase 91.x.wasm.phase-4c.2 tests — hand-assembled
    // multipart body. We don't have a live STT server to call;
    // these verify byte-exact framing so the
    // build_openai_multipart_body output matches what
    // reqwest::multipart::Form would have produced.

    #[test]
    fn multipart_body_minimum_shape() {
        let body = build_openai_multipart_body(
            "TEST-BOUNDARY",
            "whisper-1",
            b"audio-payload".to_vec(),
            "audio/ogg",
            None,
        );
        let s = String::from_utf8(body).unwrap();
        // Three sections: model, response_format, file +
        // closing boundary.
        assert_eq!(s.matches("--TEST-BOUNDARY\r\n").count(), 3);
        assert!(s.ends_with("--TEST-BOUNDARY--\r\n"));
        assert!(s.contains("name=\"model\"\r\n\r\nwhisper-1\r\n"));
        assert!(s.contains("name=\"response_format\"\r\n\r\njson\r\n"));
        assert!(s.contains(
            "name=\"file\"; filename=\"audio\"\r\n\
             Content-Type: audio/ogg\r\n\r\n\
             audio-payload\r\n"
        ));
        // No language field when lang_hint=None.
        assert!(!s.contains("name=\"language\""));
    }

    #[test]
    fn multipart_body_includes_language_when_hint_given() {
        let body = build_openai_multipart_body(
            "X",
            "m",
            b"a".to_vec(),
            "audio/ogg",
            Some("es"),
        );
        let s = String::from_utf8(body).unwrap();
        assert!(s.contains("name=\"language\"\r\n\r\nes\r\n"));
    }

    #[test]
    fn multipart_body_strips_bcp47_region_subtag() {
        let body = build_openai_multipart_body(
            "X",
            "m",
            b"a".to_vec(),
            "audio/ogg",
            Some("es-AR"),
        );
        let s = String::from_utf8(body).unwrap();
        // Whisper REST accepts language-level codes only;
        // region subtag must be dropped.
        assert!(s.contains("name=\"language\"\r\n\r\nes\r\n"));
        assert!(!s.contains("es-AR"));
        assert!(!s.contains("AR"));
    }

    #[test]
    fn multipart_body_omits_language_for_auto() {
        let body = build_openai_multipart_body(
            "X", "m", b"a".to_vec(), "audio/ogg", Some("auto"),
        );
        let s = String::from_utf8(body).unwrap();
        assert!(!s.contains("name=\"language\""));
    }

    #[test]
    fn multipart_body_omits_language_for_empty_hint() {
        let body = build_openai_multipart_body(
            "X", "m", b"a".to_vec(), "audio/ogg", Some(""),
        );
        let s = String::from_utf8(body).unwrap();
        assert!(!s.contains("name=\"language\""));
    }

    #[test]
    fn multipart_body_preserves_binary_audio_verbatim() {
        // Embed every byte 0x00..=0xFF — any naive string
        // handling would corrupt this.
        let audio: Vec<u8> = (0u8..=255).collect();
        let body = build_openai_multipart_body(
            "X",
            "m",
            audio.clone(),
            "audio/L16",
            None,
        );
        // Find the start of the audio payload — after the
        // "\r\n\r\n" that ends the file field header.
        let needle = b"Content-Type: audio/L16\r\n\r\n";
        let idx = body
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("file field header must be present");
        let audio_start = idx + needle.len();
        let audio_end = audio_start + audio.len();
        assert_eq!(&body[audio_start..audio_end], audio.as_slice());
        // CRLF after the audio payload, then closing boundary.
        assert_eq!(&body[audio_end..audio_end + 2], b"\r\n");
    }
}
