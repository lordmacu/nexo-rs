//! Voice-mode reply pipeline for microapps.
//!
//! Lifts the agent-creator-microapp's `voice_mode` infrastructure
//! into reusable pieces so any microapp building a TTS-backed
//! reply transformer composes them instead of copying ~1500 LOC:
//!
//! - [`store::VoiceModeStore`] — SQLite-backed per-conversation
//!   toggle (cloneable; rides on the firehose `SqlitePool`).
//! - [`normalize`] — markdown → sentence-boundary normaliser,
//!   emoji / unsafe-char stripper, XML-aware punctuation collapser.
//! - [`ssml`] — operator marker translator (`[pause]`, `[em]`,
//!   `[strong]`, `[spell]`, `[slow]`, `[fast]`) + auto-detect
//!   pipeline (ISO + slash dates, currencies, big cardinals,
//!   acronyms with denylist).
//! - [`tts::TtsProvider`] trait + [`tts::EdgeTtsProvider`] (Edge
//!   Read-Aloud websocket impl) + [`tts::transcode_mp3_to_opus_ogg`]
//!   ffmpeg pipe.
//! - [`tts::synthesize_voice_note`] — end-to-end pipeline
//!   composing every piece above.
//!
//! HTTP toggle routes (the actual `GET/PUT /api/voice_mode`
//! handlers) stay in the microapp because axum + serde wiring
//! varies by app. The SDK supplies the store; the microapp wires
//! the handlers in 60 LOC.

pub mod locale_addenda;
pub mod normalize;
pub mod ssml;
pub mod store;
pub mod tts;

use thiserror::Error;

pub use locale_addenda::{
    default_voice_for_locale, language_style_addendum, voice_mode_addendum,
};
pub use store::{VoiceModeRow, VoiceModeStore, DEFAULT_VOICE_ID};
pub use tts::{
    synthesize_voice_note, transcode_mp3_to_opus_ogg, EdgeTtsProvider, TtsProvider, VoiceNote,
    EDGE_AUDIO_FORMAT, VOICE_NOTE_MIME,
};

/// Result alias for voice-mode operations.
pub type Result<T> = std::result::Result<T, VoiceError>;

/// Typed error surface for the `voice` feature.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum VoiceError {
    /// SQLite operation failed (open, DDL, INSERT, SELECT).
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),

    /// Microsoft Edge synthesis call failed (connect, websocket
    /// closed, bad voice id, …). Payload is the operator-facing
    /// reason.
    #[error("edge: {0}")]
    Edge(String),

    /// `ffmpeg` exited non-zero or produced empty output.
    #[error("ffmpeg: {0}")]
    Ffmpeg(String),

    /// Body became empty after the SSML / strip / collapse
    /// pipeline. Caller usually falls back to the original text.
    #[error("empty text — nothing to synthesize")]
    EmptyText,

    /// Caller passed an empty `voice_id`.
    #[error("empty voice_id")]
    EmptyVoiceId,

    /// Provider accepted the request and returned 0 bytes after
    /// the plain-text fallback also failed (Edge endpoint flake).
    #[error("synthesis returned 0 bytes (provider flake)")]
    EmptySynthesis,
}
