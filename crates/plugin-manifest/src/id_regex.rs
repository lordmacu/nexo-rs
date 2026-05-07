//! Phase 81.13 — shared plugin id regex + reserved id list.
//!
//! Before 81.13 these constants were duplicated across
//! `nexo-extensions::manifest` (legacy `^[a-z][a-z0-9_-]*$`,
//! up to `MAX_ID_LEN = 64`) and `nexo-plugin-manifest::validate`
//! (stricter `^[a-z][a-z0-9_]{0,31}$`, length 32, no hyphens).
//! The mismatch broke the bridge between the two manifest
//! parsers — a plugin id like `agent-creator` validated under
//! the legacy regex but failed under the modern one, so admin
//! capabilities collected from `nexo-plugin.toml` never matched
//! the extension id collected from `plugin.toml`.
//!
//! 81.13 unifies on a single relaxed regex
//! (`^[a-z][a-z0-9_-]{0,63}$`, length 64) that accepts every id
//! the in-tree plugins use today. Both crates import this
//! module instead of carrying their own copies.

use std::sync::OnceLock;

use regex::Regex;

use crate::error::ManifestError;

/// Canonical plugin id regex source.
///
/// - Lowercase only.
/// - Starts with a letter.
/// - Body characters: lowercase, digits, `_`, `-`.
/// - Length 1..=64.
pub const ID_REGEX_SRC: &str = r"^[a-z][a-z0-9_-]{0,63}$";

/// Hard upper bound on plugin id length. Enforced ahead of the
/// regex so error messages mention "too long" instead of a
/// generic "doesn't match".
pub const MAX_ID_LEN: usize = 64;

/// Compiled `ID_REGEX_SRC` cached for the process lifetime.
/// Construction is cheap but cached anyway — every plugin id
/// validation hits this on the boot path.
pub fn id_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(ID_REGEX_SRC).expect("valid id regex"))
}

/// Built-in plugin ids reserved by the framework. Operators
/// cannot register an extension claiming one of these, even if
/// the corresponding native crate isn't loaded on a given
/// deployment.
///
/// Moved from `nexo-extensions::manifest::RESERVED_IDS` as part
/// of 81.13 to avoid the same list living in two places.
pub const RESERVED_IDS: &[&str] = &[
    "agent",
    "browser",
    "core",
    "email",
    "heartbeat",
    "memory",
    "telegram",
    "whatsapp",
];

/// Host-side dynamic extension of [`RESERVED_IDS`]. The host
/// binary calls [`register_reserved_ids`] once at startup with
/// any native crate ids that aren't baked into the static list.
/// `is_reserved_id` unions both sources.
static DYNAMIC_RESERVED_IDS: OnceLock<Vec<String>> = OnceLock::new();

/// Idempotent registration. First call wins; subsequent calls
/// return `Err(&'static str)` so the caller can log + move on.
pub fn register_reserved_ids(ids: impl IntoIterator<Item = String>) -> Result<(), &'static str> {
    let mut v: Vec<String> = ids.into_iter().collect();
    v.sort();
    v.dedup();
    DYNAMIC_RESERVED_IDS
        .set(v)
        .map_err(|_| "reserved ids already initialized")
}

/// `true` when `id` is reserved by either the static list or the
/// host-registered dynamic set.
pub fn is_reserved_id(id: &str) -> bool {
    if RESERVED_IDS.contains(&id) {
        return true;
    }
    DYNAMIC_RESERVED_IDS
        .get()
        .map(|v| v.iter().any(|s| s == id))
        .unwrap_or(false)
}

/// Validate a plugin id against the unified rules:
/// - length within [`MAX_ID_LEN`]
/// - matches [`ID_REGEX_SRC`]
/// - not in [`RESERVED_IDS`] / dynamic reserved set
///
/// Returns the *first* failure as
/// [`ManifestError::IdInvalid`] so error messages stay focused.
/// Use this from manifest validators to keep the rules
/// consistent across `nexo-plugin-manifest` and
/// `nexo-extensions`.
pub fn validate_id(id: &str) -> Result<(), ManifestError> {
    if id.len() > MAX_ID_LEN {
        return Err(ManifestError::IdInvalid {
            id: id.to_string(),
            reason: "exceeds max length 64",
        });
    }
    if !id_regex().is_match(id) {
        return Err(ManifestError::IdInvalid {
            id: id.to_string(),
            reason: "must match `^[a-z][a-z0-9_-]{0,63}$`",
        });
    }
    if is_reserved_id(id) {
        return Err(ManifestError::IdInvalid {
            id: id.to_string(),
            reason: "reserved by the framework",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_regex_accepts_hyphens() {
        assert!(id_regex().is_match("agent-creator"));
        assert!(id_regex().is_match("template-rust"));
        assert!(id_regex().is_match("video-frames"));
    }

    #[test]
    fn id_regex_accepts_underscores() {
        assert!(id_regex().is_match("agent_creator"));
        assert!(id_regex().is_match("template_plugin_typescript"));
    }

    #[test]
    fn id_regex_rejects_uppercase() {
        assert!(!id_regex().is_match("Agent-Creator"));
        assert!(!id_regex().is_match("AGENT_CREATOR"));
    }

    #[test]
    fn id_regex_rejects_starts_with_digit() {
        assert!(!id_regex().is_match("1foo"));
        assert!(!id_regex().is_match("9_plugin"));
    }

    #[test]
    fn id_regex_rejects_starts_with_hyphen() {
        assert!(!id_regex().is_match("-agent"));
        assert!(!id_regex().is_match("_agent"));
    }

    #[test]
    fn id_regex_max_64_chars() {
        let id_64 = "a".repeat(64);
        let id_65 = "a".repeat(65);
        // 64 chars matches.
        assert!(id_regex().is_match(&id_64));
        // 65 chars doesn't match the body length, so the regex
        // rejects it independently of the explicit length check
        // in `validate_id`.
        assert!(!id_regex().is_match(&id_65));
    }

    #[test]
    fn validate_id_reports_too_long_separately_from_regex() {
        let err = validate_id(&"a".repeat(65)).unwrap_err();
        match err {
            ManifestError::IdInvalid { reason, .. } => {
                assert!(reason.contains("max length"), "got: {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn validate_id_reports_regex_mismatch() {
        let err = validate_id("Agent-Creator").unwrap_err();
        match err {
            ManifestError::IdInvalid { reason, .. } => {
                assert!(reason.contains("must match"), "got: {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn validate_id_rejects_reserved() {
        let err = validate_id("browser").unwrap_err();
        match err {
            ManifestError::IdInvalid { reason, .. } => {
                assert!(reason.contains("reserved"), "got: {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn validate_id_accepts_valid_id() {
        validate_id("agent-creator").expect("valid");
        validate_id("agent_creator").expect("valid");
        validate_id("ventas_etb").expect("valid");
    }

    #[test]
    fn reserved_ids_static_list_is_locked_down() {
        // Locking the wire contract — adding/removing a reserved
        // id is a semver-major change for plugin authors.
        let expected: &[&str] = &[
            "agent",
            "browser",
            "core",
            "email",
            "heartbeat",
            "memory",
            "telegram",
            "whatsapp",
        ];
        assert_eq!(RESERVED_IDS, expected);
    }

    #[test]
    fn is_reserved_id_covers_static_list() {
        assert!(is_reserved_id("browser"));
        assert!(is_reserved_id("whatsapp"));
        assert!(!is_reserved_id("agent-creator"));
        assert!(!is_reserved_id("ventas-etb"));
    }
}
