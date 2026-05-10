//! Local speech-to-text for inbound voice notes.
//!
//! Lifts `agent-creator-microapp::stt` into a reusable SDK
//! feature. Two pieces:
//!
//! - [`transcribe::transcribe_file`] — decodes an ogg-opus audio
//!   file (the format both WhatsApp and Telegram voice notes use)
//!   to 16 kHz mono s16le PCM via `ogg` + `opus-wave` (pure Rust,
//!   no `ffmpeg` subprocess) and runs whisper.cpp on the result.
//! - [`tool::InboundTransformHandler`] — `ToolHandler` impl ready to
//!   hand to `Microapp::with_tool("audio_stt_inbound_transform", …)`.
//!   Implements the framework's auto-discovered
//!   `*_inbound_transform` wire shape (see `nexo-core`'s LLM behavior
//!   pipeline): non-audio passthrough, missing-file → graceful
//!   `{ ok: false }`, transcription failure → passthrough so the
//!   chat turn isn't dropped.
//!
//! # Quick start
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use nexo_microapp_sdk::Microapp;
//! # use nexo_microapp_sdk::stt::{InboundTransformHandler, TranscribeConfig};
//! let cfg = Arc::new(TranscribeConfig {
//!     model_path: "/var/lib/myapp/whisper/ggml-tiny-q5_1.bin".into(),
//!     lang_hint: Some("es".into()),
//!     ..Default::default()
//! });
//! let app = Microapp::new("voice-microapp", env!("CARGO_PKG_VERSION"))
//!     .with_tool(
//!         "audio_stt_inbound_transform",
//!         InboundTransformHandler::new(cfg),
//!     );
//! # let _ = app;
//! ```

pub mod tool;
pub mod transcribe;
// Phase 91 — Candle backend module tree. Cfg-gated to avoid
// pulling Candle deps into builds that only enable `stt`
// (whisper-rs legacy). The submodules themselves carry the same
// `#[cfg(feature = "stt-candle")]` guard so a stale build cache
// doesn't surface unrelated errors when toggling features.
#[cfg(feature = "stt-candle")]
pub(crate) mod mel;

use std::path::PathBuf;

use thiserror::Error;

pub use tool::InboundTransformHandler;
pub use transcribe::transcribe_file;

/// Result alias for STT operations.
pub type Result<T> = std::result::Result<T, SttError>;

/// Typed error surface for the `stt` feature.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SttError {
    /// I/O failed (file read, ffmpeg pipe, etc.).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Legacy variant, kept for backward compatibility with
    /// callers that pattern-match on it. New code should use
    /// [`Self::Decode`] / [`Self::UnsupportedFormat`] instead.
    #[deprecated(note = "ffmpeg is no longer used; map to Decode/UnsupportedFormat")]
    #[error("ffmpeg: {0}")]
    Ffmpeg(String),

    /// Audio decode failed inside the pure-Rust pipeline (ogg
    /// demux error, opus packet rejected, sample-rate mismatch,
    /// etc.).
    #[error("decode: {0}")]
    Decode(String),

    /// The audio is in a container/codec the SDK doesn't decode.
    /// The personal-assistant build only supports ogg-opus
    /// (WhatsApp + Telegram voice notes); add a wider decoder
    /// behind a feature flag if email attachments must be
    /// transcribed.
    #[error("unsupported audio format: {0}")]
    UnsupportedFormat(String),

    /// whisper.cpp reported an error during context init,
    /// `state::full`, or segment retrieval.
    #[error("whisper: {0}")]
    Whisper(String),

    /// The requested whisper model file is not on disk. The
    /// recommended fix is operator action (download the
    /// checkpoint), not auto-fetch — boot paths shouldn't pull
    /// arbitrary URLs.
    #[error("model not found: {0}")]
    ModelMissing(String),

    /// Decoded audio was empty (silence, or ffmpeg dropped
    /// everything). Distinguished from [`Self::Ffmpeg`] so the
    /// caller can decide whether to log loudly or skip silently.
    #[error("decoded audio is empty")]
    EmptyAudio,

    /// whisper produced an empty transcript. Likely silence or a
    /// language mismatch with the hint.
    #[error("whisper produced empty transcript")]
    EmptyTranscript,
}

/// Knobs for [`transcribe_file`] / [`InboundTransformHandler`].
///
/// Cheap to clone and intended to be wrapped in `Arc` and shared
/// across handler invocations.
#[derive(Debug, Clone)]
pub struct TranscribeConfig {
    /// Absolute path to the whisper model file (`.bin` GGML format).
    /// The file is opened on the FIRST transcription, not at
    /// construction — so a missing file surfaces as a runtime
    /// [`SttError::ModelMissing`] when the first voice note lands,
    /// not at boot.
    pub model_path: PathBuf,

    /// BCP-47 language hint passed to whisper. `None` lets whisper
    /// auto-detect (slower per turn but useful for multilingual
    /// bots). Common values: `Some("es")`, `Some("en")`.
    pub lang_hint: Option<String>,

    /// Legacy field — the pipeline no longer spawns `ffmpeg`, so
    /// this value is ignored. Kept as a `pub` field so callers
    /// that destructured `TranscribeConfig` still compile.
    #[deprecated(note = "ignored — the SDK decodes ogg-opus in pure Rust now")]
    pub ffmpeg_path: PathBuf,

    /// Target sample rate for the PCM stream fed to whisper.
    /// Defaults to 16 000 Hz (the rate whisper.cpp is trained on);
    /// other rates produce garbage.
    pub target_sample_rate: u32,
}

impl Default for TranscribeConfig {
    /// Defaults assume the operator places the model at
    /// `./data/whisper/ggml-tiny-q5_1.bin`. Microapps usually
    /// override `model_path` from a state-root env var.
    #[allow(deprecated)] // populates the legacy `ffmpeg_path` field
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("./data/whisper/ggml-tiny-q5_1.bin"),
            lang_hint: None,
            ffmpeg_path: PathBuf::from("ffmpeg"),
            target_sample_rate: 16_000,
        }
    }
}
