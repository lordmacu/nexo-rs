//! Per-locale system-prompt addenda + voice picker tables.
//!
//! Phase voice-plugin-extract Step 3 — content moved to
//! `nexo_tool_meta::voice_addenda` so the upcoming
//! `nexo-plugin-voice` plus wizards, personas, and any other
//! consumer can share it without pulling the SDK (sqlx + the
//! voice synth pipeline) into their compile graph.
//!
//! This shim re-exports the three pub fns so existing
//! `nexo_microapp_sdk::voice::{voice_mode_addendum,
//! language_style_addendum, default_voice_for_locale}` imports
//! keep compiling. Slated for removal in Phase Q (SDK voice
//! pipeline cleanup) once consumers migrate to the
//! `nexo-plugin-voice` crate.

pub use nexo_tool_meta::voice_addenda::{
    default_voice_for_locale, language_style_addendum, voice_mode_addendum,
};
