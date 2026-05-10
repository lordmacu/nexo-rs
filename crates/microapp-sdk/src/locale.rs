//! Phase 81.19.b locale follow-up — `Locale`, `LangCode`,
//! `RegionCode`, and `LocaleParseError` were moved to
//! `nexo-tool-meta` so `nexo-config` can validate
//! `agents.<id>.language` at YAML deserialise time without
//! triggering a layer inversion (`nexo-config` cannot depend on
//! `nexo-microapp-sdk`).
//!
//! This module is now a thin re-export shim. Existing call sites
//! that import `nexo_microapp_sdk::Locale`,
//! `nexo_microapp_sdk::locale::Locale`, etc. keep working
//! unchanged.

pub use nexo_tool_meta::locale::*;
