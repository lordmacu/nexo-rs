//! Per-locale system-prompt addenda + voice picker tables.
//!
//! Phase voice-plugin-extract Step 1 — content lives in
//! [`locale_addenda`] (lifted verbatim from the previous
//! `nexo-microapp-sdk::voice::locale_addenda` home so wizard,
//! personas, and the upcoming `nexo-plugin-voice` can all share
//! one copy without coupling through the SDK).
//!
//! Three pub functions consume [`crate::Locale`] and return
//! `&'static str` content the framework wires into the LLM turn:
//!
//! - [`voice_mode_addendum`] — SSML-marker tutorial that the
//!   voice-mode inbound transform plugs into the per-turn system
//!   prompt when voice mode is active for the conversation.
//! - [`language_style_addendum`] — dialect / vocabulary guidance
//!   appended on every turn (independent of voice mode).
//! - [`default_voice_for_locale`] — Microsoft Edge voice id matched
//!   to the locale. Falls back to the language-only canonical, then
//!   to [`DEFAULT_VOICE_ID`].

mod locale_addenda;

pub use locale_addenda::{default_voice_for_locale, language_style_addendum, voice_mode_addendum};

/// Default Microsoft Edge voice when the operator hasn't picked
/// one yet. Spanish-Mexico female; works well for LATAM operators
/// and matches the most common WhatsApp-bot use case.
///
/// Single source of truth for every consumer (plugin, wizard,
/// personas). Previously lived in
/// `nexo-microapp-sdk::voice::store::DEFAULT_VOICE_ID`; relocated
/// here so anything outside the SDK can use it without pulling
/// `sqlx` + the full SDK in.
pub const DEFAULT_VOICE_ID: &str = "es-MX-DaliaNeural";
