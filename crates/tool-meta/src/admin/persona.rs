//! `nexo/admin/persona/*` wire types.
//!
//! Phase 81.31 — plugin-driven persona multi-locale. The admin
//! reads per-agent localised persona content (system_prompt +
//! IDENTITY/SOUL/USER/AGENTS workspace files) via `agents/get`
//! (which now includes [`PersonaLocales`]) and writes new variants
//! via `nexo/admin/persona/save_localized` carrying
//! [`PersonaSaveLocalizedRequest`].
//!
//! Workspace file convention: `<workspace>/<FILE>.<locale>.md`
//! (e.g. `IDENTITY.es.md`). Files without a suffix
//! (`IDENTITY.md`) act as the default / `en` fallback so
//! pre-Phase-81.31 personas keep working unchanged.

use serde::{Deserialize, Serialize};

/// Catalog of localised persona variants for one agent. Returned
/// inside [`crate::admin::agents::AgentDetail::persona_locales`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct PersonaLocales {
    /// Ordered BCP-47 locale tags. First entry is the agent's
    /// configured `language` (fallback `"en"`); remaining entries
    /// are alpha-sorted. Always non-empty.
    pub available: Vec<String>,
    /// Per-locale snapshot of the agent's persona content.
    /// Implemented as a `Vec<(locale, snapshot)>` so the wire
    /// stays an ordered list (matches `available`) without needing
    /// the admin to re-sort.
    pub snapshots: Vec<PersonaLocaleEntry>,
}

/// One `(locale, snapshot)` pair inside [`PersonaLocales::snapshots`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct PersonaLocaleEntry {
    /// BCP-47 locale tag matching one entry in
    /// [`PersonaLocales::available`].
    pub locale: String,
    /// Snapshot of the persona content for this locale.
    pub snapshot: PersonaSnapshot,
    /// Phase 81.31 follow-up #2 — recommended Microsoft Edge
    /// neural voice id for this locale (e.g. `"es-AR-ElenaNeural"`,
    /// `"en-US-AriaNeural"`). Sourced from
    /// `nexo_tool_meta::locale::default_voice_for_locale`. `None`
    /// when the daemon couldn't parse the locale or no voice is
    /// mapped. Surfaced read-only — operator overrides via the
    /// per-conversation voice mode tool, not via this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_voice: Option<String>,
}

/// Snapshot of one agent's persona content at a specific locale.
///
/// Empty strings indicate the file is absent on disk AND no
/// fallback is available; admin renders "(not localized)" hints
/// for files not in [`Self::present_files`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct PersonaSnapshot {
    /// Resolved system prompt (from `locale_prompts[locale]` or
    /// the top-level `system_prompt` fallback).
    pub system_prompt: String,
    /// Content of `<workspace>/IDENTITY[.<locale>].md`.
    pub identity: String,
    /// Content of `<workspace>/SOUL[.<locale>].md`.
    pub soul: String,
    /// Content of `<workspace>/USER[.<locale>].md`.
    pub user: String,
    /// Content of `<workspace>/AGENTS[.<locale>].md`.
    pub agents: String,
    /// Subset of `{"system_prompt","identity","soul","user","agents"}`
    /// whose source actually exists for this locale (either as a
    /// locale-suffixed file or — for non-default locales — falling
    /// back to the unsuffixed file). Used by the admin to render
    /// "(not localized)" hints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub present_files: Vec<String>,
}

/// Params for `nexo/admin/persona/save_localized`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct PersonaSaveLocalizedRequest {
    /// Target agent id. Must exist in `agents.yaml` (or a
    /// persona-shipped `agents.d/*.yaml`).
    pub agent_id: String,
    /// BCP-47 locale tag. Validated server-side via
    /// `nexo_tool_meta::locale::Locale::from_str` before any
    /// filesystem write.
    pub locale: String,
    /// New system_prompt for this locale. Written into
    /// `agents.d/<id>.yaml::locale_prompts[locale]` when
    /// `patch_yaml` is true.
    pub system_prompt: String,
    /// New `IDENTITY.<locale>.md` content.
    pub identity: String,
    /// New `SOUL.<locale>.md` content.
    pub soul: String,
    /// New `USER.<locale>.md` content.
    pub user: String,
    /// New `AGENTS.<locale>.md` content.
    pub agents: String,
    /// When `true` (default), the daemon patches
    /// `agents.d/<id>.yaml::locale_prompts[locale]` after writing
    /// the four workspace files. Set `false` when the caller
    /// already manages the YAML separately (avoids a double-write
    /// race when followed by `agents/upsert`).
    #[serde(default = "default_true")]
    pub patch_yaml: bool,
}

fn default_true() -> bool {
    true
}

/// Response shape for `nexo/admin/persona/save_localized`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct PersonaSaveLocalizedResponse {
    /// Absolute paths of files successfully written, in write
    /// order. Useful for rollback by the caller and for audit
    /// logging.
    pub written_paths: Vec<String>,
    /// Updated [`PersonaLocales`] reflecting the new locale.
    /// Lets the admin refresh its dropdown without a second
    /// `agents/get` roundtrip.
    pub persona_locales: PersonaLocales,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips() {
        let s = PersonaSnapshot {
            system_prompt: "top".into(),
            identity: "i".into(),
            soul: "s".into(),
            user: "u".into(),
            agents: "a".into(),
            present_files: vec!["system_prompt".into(), "identity".into()],
        };
        let v = serde_json::to_value(&s).unwrap();
        let back: PersonaSnapshot = serde_json::from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn locales_round_trip_preserves_order() {
        let l = PersonaLocales {
            available: vec!["es".into(), "en".into(), "pt-BR".into()],
            snapshots: vec![
                PersonaLocaleEntry {
                    locale: "es".into(),
                    recommended_voice: None,
                    snapshot: PersonaSnapshot {
                        system_prompt: "spanish".into(),
                        ..Default::default()
                    },
                },
                PersonaLocaleEntry {
                    locale: "en".into(),
                    recommended_voice: None,
                    snapshot: PersonaSnapshot {
                        system_prompt: "english".into(),
                        ..Default::default()
                    },
                },
            ],
        };
        let v = serde_json::to_value(&l).unwrap();
        let back: PersonaLocales = serde_json::from_value(v).unwrap();
        assert_eq!(l, back);
        assert_eq!(back.available[0], "es");
    }

    #[test]
    fn save_request_round_trips_with_default_patch_yaml() {
        let req = PersonaSaveLocalizedRequest {
            agent_id: "cody".into(),
            locale: "es".into(),
            system_prompt: "p".into(),
            identity: "i".into(),
            soul: "s".into(),
            user: "u".into(),
            agents: "a".into(),
            patch_yaml: true,
        };
        let v = serde_json::to_value(&req).unwrap();
        let back: PersonaSaveLocalizedRequest = serde_json::from_value(v).unwrap();
        assert_eq!(req, back);
        // patch_yaml absent from wire defaults to true
        let raw = serde_json::json!({
            "agent_id": "x", "locale": "en",
            "system_prompt": "", "identity": "", "soul": "", "user": "", "agents": ""
        });
        let parsed: PersonaSaveLocalizedRequest = serde_json::from_value(raw).unwrap();
        assert!(parsed.patch_yaml);
    }

    #[test]
    fn save_response_skips_empty_paths() {
        let r = PersonaSaveLocalizedResponse::default();
        let v = serde_json::to_value(&r).unwrap();
        let back: PersonaSaveLocalizedResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
    }
}
