//! Phase 81.31 — persona multi-locale filesystem helpers.
//!
//! Workspace convention for localised persona content:
//!
//! ```text
//! <workspace_dir>/
//!   IDENTITY.md           ← default / `en` fallback
//!   IDENTITY.es.md        ← Spanish variant
//!   SOUL.md
//!   SOUL.es.md
//!   USER.md
//!   USER.es.md
//!   AGENTS.md
//!   AGENTS.es.md
//! ```
//!
//! Files without a locale suffix act as the default; missing
//! `<FILE>.<locale>.md` falls back to `<FILE>.md`. Empty string
//! result means the operator's persona doesn't ship that section.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use nexo_tool_meta::admin::persona::{PersonaLocaleEntry, PersonaLocales, PersonaSnapshot};

/// Workspace files the persona system surfaces. Order is the wire
/// order of [`PersonaSnapshot`] fields — UI relies on it for
/// "(not localized)" hints.
pub const PERSONA_FILES: &[(&str, &str)] = &[
    ("identity", "IDENTITY"),
    ("soul", "SOUL"),
    ("user", "USER"),
    ("agents", "AGENTS"),
];

/// Errors surfaced by [`write_persona_snapshot`].
#[derive(Debug, Error)]
pub enum PersonaWriteError {
    /// Locale tag failed BCP-47 validation.
    #[error("invalid locale {locale:?}: {message}")]
    InvalidLocale {
        /// Operator-supplied tag that failed validation.
        locale: String,
        /// Message from `Locale::from_str`.
        message: String,
    },
    /// Filesystem I/O failed.
    #[error("io error on {path}: {source}")]
    Io {
        /// Path that triggered the error.
        path: PathBuf,
        /// Underlying io::Error.
        #[source]
        source: io::Error,
    },
}

/// Input to [`write_persona_snapshot`]. Mirrors
/// [`PersonaSnapshot`] minus the wire-only `present_files`.
#[derive(Debug, Clone)]
pub struct PersonaSnapshotInput {
    /// New IDENTITY content.
    pub identity: String,
    /// New SOUL content.
    pub soul: String,
    /// New USER content.
    pub user: String,
    /// New AGENTS content.
    pub agents: String,
}

/// Discover every locale this agent has persona content for.
///
/// Order: agent's `language` (or `"en"` when absent/empty) first,
/// then every other locale found in `locale_prompts` keys + every
/// `<FILE>.<locale>.md` suffix found under `workspace_dir`, alpha
/// sorted, deduplicated. Always returns at least one entry.
pub fn discover_agent_locales(
    workspace_dir: &Path,
    locale_prompts_keys: &[String],
    agent_language: Option<&str>,
) -> Vec<String> {
    let mut all: BTreeSet<String> = BTreeSet::new();
    for k in locale_prompts_keys {
        all.insert(k.clone());
    }
    for tag in scan_workspace_locale_suffixes(workspace_dir) {
        all.insert(tag);
    }

    let primary = agent_language
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "en".to_string());
    all.insert(primary.clone());

    let mut rest: Vec<String> = all.into_iter().filter(|t| t != &primary).collect();
    rest.sort();

    let mut out = Vec::with_capacity(rest.len() + 1);
    out.push(primary);
    out.extend(rest);
    out
}

/// Read one locale's persona snapshot. For each persona file:
///   1. `<workspace_dir>/<FILE>.<locale>.md` if present
///   2. else `<workspace_dir>/<FILE>.md` (unsuffixed = default
///      shared by every locale)
///   3. else empty string
///
/// `present_files` tracks which sections actually exist on disk
/// for the requested locale (after fallback resolution). The
/// `system_prompt` slot is filled from `locale_prompts[locale]`
/// when present, falling back to `top_system_prompt`.
pub fn read_persona_snapshot(
    workspace_dir: &Path,
    locale: &str,
    top_system_prompt: &str,
    locale_prompts: &BTreeMap<String, String>,
) -> PersonaSnapshot {
    let mut snap = PersonaSnapshot::default();
    let mut present: Vec<String> = Vec::new();

    // system_prompt — prefer locale variant.
    if let Some(p) = locale_prompts.get(locale) {
        snap.system_prompt = p.clone();
        present.push("system_prompt".to_string());
    } else if !top_system_prompt.is_empty() {
        snap.system_prompt = top_system_prompt.to_string();
        // `present` only includes the slot when a locale-specific
        // value was found — the default top-level prompt counts
        // as fallback, not localised content.
    }

    for (key, stem) in PERSONA_FILES {
        let (content, found_localized) = read_file_with_fallback(workspace_dir, stem, locale);
        match *key {
            "identity" => snap.identity = content,
            "soul" => snap.soul = content,
            "user" => snap.user = content,
            "agents" => snap.agents = content,
            _ => unreachable!("persona file key {key:?} unhandled"),
        }
        if found_localized {
            present.push((*key).to_string());
        }
    }

    snap.present_files = present;
    snap
}

/// Build a complete [`PersonaLocales`] for one agent. Reads each
/// locale's snapshot in order returned by
/// [`discover_agent_locales`].
pub fn build_persona_locales(
    workspace_dir: &Path,
    top_system_prompt: &str,
    locale_prompts: &BTreeMap<String, String>,
    agent_language: Option<&str>,
) -> PersonaLocales {
    let keys: Vec<String> = locale_prompts.keys().cloned().collect();
    let available = discover_agent_locales(workspace_dir, &keys, agent_language);
    let snapshots = available
        .iter()
        .map(|loc| {
            // Phase 81.31 follow-up #2 — surface the recommended
            // Edge TTS voice for each locale. Parse failures
            // collapse to `None` so the wizard shows "no
            // recommendation" instead of guessing.
            use std::str::FromStr;
            let recommended_voice = nexo_tool_meta::locale::Locale::from_str(loc)
                .ok()
                .map(|l| nexo_tool_meta::locale::default_voice_for_locale(Some(&l)).to_string());
            PersonaLocaleEntry {
                locale: loc.clone(),
                snapshot: read_persona_snapshot(
                    workspace_dir,
                    loc,
                    top_system_prompt,
                    locale_prompts,
                ),
                recommended_voice,
            }
        })
        .collect();
    PersonaLocales {
        available,
        snapshots,
    }
}

/// Write the four workspace files with a locale suffix. Each file
/// is written via temp+rename to keep readers consistent. The
/// returned list contains the absolute paths of every file
/// actually written so the caller can roll back on partial
/// failure (the function itself does NOT attempt rollback —
/// callers compose with the YAML patch for full atomicity).
pub fn write_persona_snapshot(
    workspace_dir: &Path,
    locale: &str,
    snap: &PersonaSnapshotInput,
) -> Result<Vec<PathBuf>, PersonaWriteError> {
    use std::str::FromStr;
    nexo_tool_meta::locale::Locale::from_str(locale).map_err(|e| {
        PersonaWriteError::InvalidLocale {
            locale: locale.to_string(),
            message: format!("{e}"),
        }
    })?;
    fs::create_dir_all(workspace_dir).map_err(|e| PersonaWriteError::Io {
        path: workspace_dir.to_path_buf(),
        source: e,
    })?;

    let entries: [(&str, &str); 4] = [
        ("IDENTITY", snap.identity.as_str()),
        ("SOUL", snap.soul.as_str()),
        ("USER", snap.user.as_str()),
        ("AGENTS", snap.agents.as_str()),
    ];

    let mut written: Vec<PathBuf> = Vec::with_capacity(4);
    for (stem, content) in entries {
        let dst = workspace_dir.join(format!("{stem}.{locale}.md"));
        write_atomic(&dst, content)?;
        written.push(dst);
    }
    Ok(written)
}

// ── Internals ──────────────────────────────────────────────────

fn read_file_with_fallback(workspace_dir: &Path, stem: &str, locale: &str) -> (String, bool) {
    let localized = workspace_dir.join(format!("{stem}.{locale}.md"));
    if let Ok(s) = fs::read_to_string(&localized) {
        return (s, true);
    }
    let plain = workspace_dir.join(format!("{stem}.md"));
    if let Ok(s) = fs::read_to_string(&plain) {
        return (s, false);
    }
    (String::new(), false)
}

fn scan_workspace_locale_suffixes(workspace_dir: &Path) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let Ok(entries) = fs::read_dir(workspace_dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // `IDENTITY.es.md` → split into ["IDENTITY", "es", "md"]
        let parts: Vec<&str> = name.split('.').collect();
        if parts.len() != 3 || parts[2] != "md" {
            continue;
        }
        let (stem, locale) = (parts[0], parts[1]);
        if !PERSONA_FILES.iter().any(|(_, s)| *s == stem) {
            continue;
        }
        // Quick BCP-47 sanity — skip if the locale string fails
        // the validator. Avoids polluting `available` with
        // accidental files like `IDENTITY.draft.md`.
        use std::str::FromStr;
        if nexo_tool_meta::locale::Locale::from_str(locale).is_ok() {
            out.insert(locale.to_string());
        }
    }
    out.into_iter().collect()
}

fn write_atomic(dst: &Path, content: &str) -> Result<(), PersonaWriteError> {
    let tmp = dst.with_extension("md.tmp");
    fs::write(&tmp, content).map_err(|e| PersonaWriteError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    fs::rename(&tmp, dst).map_err(|e| PersonaWriteError::Io {
        path: dst.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn discover_orders_language_first_then_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let keys = vec!["en".to_string(), "es".to_string(), "pt-BR".to_string()];
        let out = discover_agent_locales(dir.path(), &keys, Some("es"));
        assert_eq!(out, vec!["es", "en", "pt-BR"]);
    }

    #[test]
    fn discover_falls_back_to_en_when_language_empty() {
        let dir = tempfile::tempdir().unwrap();
        let out = discover_agent_locales(dir.path(), &[], None);
        assert_eq!(out, vec!["en"]);
    }

    #[test]
    fn discover_picks_up_filesystem_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("IDENTITY.fr.md"), "fr").unwrap();
        std::fs::write(dir.path().join("SOUL.de.md"), "de").unwrap();
        // Ignored: not a persona file
        std::fs::write(dir.path().join("RANDOM.es.md"), "x").unwrap();
        let out = discover_agent_locales(dir.path(), &["en".into()], Some("en"));
        assert_eq!(out, vec!["en", "de", "fr"]);
    }

    #[test]
    fn read_snapshot_falls_back_to_unsuffixed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("IDENTITY.md"), "default identity").unwrap();
        std::fs::write(dir.path().join("SOUL.es.md"), "spanish soul").unwrap();
        let mut prompts = BTreeMap::new();
        prompts.insert("es".into(), "spanish prompt".into());
        let snap = read_persona_snapshot(dir.path(), "es", "top", &prompts);
        // identity falls back to unsuffixed (not localized)
        assert_eq!(snap.identity, "default identity");
        // soul is localized
        assert_eq!(snap.soul, "spanish soul");
        // user + agents absent
        assert_eq!(snap.user, "");
        // system_prompt from locale_prompts
        assert_eq!(snap.system_prompt, "spanish prompt");
        // present_files: system_prompt + soul only (identity is
        // fallback, not localized)
        assert!(snap.present_files.contains(&"system_prompt".to_string()));
        assert!(snap.present_files.contains(&"soul".to_string()));
        assert!(!snap.present_files.contains(&"identity".to_string()));
    }

    #[test]
    fn write_then_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let input = PersonaSnapshotInput {
            identity: "hola identity".into(),
            soul: "hola soul".into(),
            user: "hola user".into(),
            agents: "hola agents".into(),
        };
        let written = write_persona_snapshot(dir.path(), "es", &input).unwrap();
        assert_eq!(written.len(), 4);
        let mut prompts = BTreeMap::new();
        prompts.insert("es".into(), "hola prompt".into());
        let snap = read_persona_snapshot(dir.path(), "es", "fallback", &prompts);
        assert_eq!(snap.identity, "hola identity");
        assert_eq!(snap.soul, "hola soul");
        assert_eq!(snap.user, "hola user");
        assert_eq!(snap.agents, "hola agents");
    }

    #[test]
    fn write_rejects_invalid_locale() {
        let dir = tempfile::tempdir().unwrap();
        let input = PersonaSnapshotInput {
            identity: "x".into(),
            soul: "x".into(),
            user: "x".into(),
            agents: "x".into(),
        };
        let err = write_persona_snapshot(dir.path(), "klingon", &input).unwrap_err();
        match err {
            PersonaWriteError::InvalidLocale { locale, .. } => assert_eq!(locale, "klingon"),
            other => panic!("expected InvalidLocale, got {other:?}"),
        }
        // No files written.
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn build_locales_orders_snapshots_to_match_available() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("IDENTITY.md"), "en id").unwrap();
        std::fs::write(dir.path().join("IDENTITY.es.md"), "es id").unwrap();
        let mut prompts = BTreeMap::new();
        prompts.insert("en".into(), "en prompt".into());
        prompts.insert("es".into(), "es prompt".into());
        let locales = build_persona_locales(dir.path(), "top", &prompts, Some("es"));
        assert_eq!(locales.available, vec!["es", "en"]);
        assert_eq!(locales.snapshots.len(), 2);
        assert_eq!(locales.snapshots[0].locale, "es");
        assert_eq!(locales.snapshots[0].snapshot.identity, "es id");
        assert_eq!(locales.snapshots[1].locale, "en");
        assert_eq!(locales.snapshots[1].snapshot.identity, "en id");
    }

    /// Phase 81.31 follow-up #2 — `recommended_voice` populated
    /// per locale from `default_voice_for_locale`.
    #[test]
    fn build_locales_populates_recommended_voice_per_locale() {
        let dir = tempfile::tempdir().unwrap();
        let mut prompts = BTreeMap::new();
        prompts.insert("en".into(), "english".into());
        prompts.insert("es-AR".into(), "argentino".into());
        prompts.insert("pt-BR".into(), "brasileiro".into());
        let locales = build_persona_locales(dir.path(), "top", &prompts, Some("en"));
        // en → "en-US-AriaNeural" (default for language-only)
        let en = locales.snapshots.iter().find(|e| e.locale == "en").unwrap();
        assert_eq!(en.recommended_voice.as_deref(), Some("en-US-AriaNeural"));
        // es-AR → "es-AR-ElenaNeural"
        let ar = locales
            .snapshots
            .iter()
            .find(|e| e.locale == "es-AR")
            .unwrap();
        assert_eq!(ar.recommended_voice.as_deref(), Some("es-AR-ElenaNeural"));
        // pt-BR → "pt-BR-FranciscaNeural"
        let br = locales
            .snapshots
            .iter()
            .find(|e| e.locale == "pt-BR")
            .unwrap();
        assert_eq!(
            br.recommended_voice.as_deref(),
            Some("pt-BR-FranciscaNeural")
        );
    }
}
