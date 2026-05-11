//! Structural validators for [`crate::PersonaManifest`].
//! Runs *after* the typed parse — by the time we reach
//! [`validate`], the TOML is well-formed and the version is
//! v2; we only check field-level invariants the type system
//! can't express (regex shape, semver shape, path safety).
//!
//! Each violation surfaces as a [`PersonaManifestError`]
//! variant with enough context for the CLI to print an
//! actionable error.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::error::PersonaManifestError;
use crate::manifest::PersonaManifest;

/// Lazy id regex — same shape as the plugin id regex so
/// personas + plugins share namespace conventions. Lowercase
/// ascii alphanumerics + dash, 3-64 chars total, must start
/// with a non-dash.
fn id_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9-]{2,63}$").unwrap())
}

/// Lazy POSIX env-var name regex.
fn env_var_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[A-Z_][A-Z0-9_]*$").unwrap())
}

/// Run the full validator pipeline. Returns `Ok(())` only
/// when every invariant passes; the *first* failure short-
/// circuits with a typed error (no error accumulation — keeps
/// CLI output focused).
pub fn validate(manifest: &PersonaManifest) -> Result<(), PersonaManifestError> {
    validate_required_fields(&manifest.persona)?;
    validate_id(&manifest.persona.id)?;
    validate_version(&manifest.persona.version)?;
    validate_min_nexo_version(&manifest.persona.min_nexo_version)?;
    if let Some(req) = &manifest.persona.requires {
        for env in &req.env_vars {
            validate_env_var(env.name.as_str(), env.description.as_str())?;
        }
    }
    if let Some(contrib) = &manifest.persona.contributes {
        validate_contributes(contrib)?;
    }
    Ok(())
}

fn validate_required_fields(p: &crate::manifest::PersonaSection) -> Result<(), PersonaManifestError> {
    let checks: [(&'static str, &str); 4] = [
        ("id", &p.id),
        ("version", &p.version),
        ("description", &p.description),
        ("min_nexo_version", &p.min_nexo_version),
    ];
    for (field, value) in checks {
        if value.trim().is_empty() {
            return Err(PersonaManifestError::EmptyRequiredField { field });
        }
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), PersonaManifestError> {
    if id_regex().is_match(id) {
        Ok(())
    } else {
        Err(PersonaManifestError::InvalidId { got: id.to_string() })
    }
}

fn validate_version(v: &str) -> Result<(), PersonaManifestError> {
    semver::Version::parse(v)
        .map(|_| ())
        .map_err(|e| PersonaManifestError::InvalidVersion {
            got: v.to_string(),
            reason: e.to_string(),
        })
}

fn validate_min_nexo_version(v: &str) -> Result<(), PersonaManifestError> {
    semver::VersionReq::parse(v)
        .map(|_| ())
        .map_err(|e| PersonaManifestError::InvalidMinNexoVersion {
            got: v.to_string(),
            reason: e.to_string(),
        })
}

fn validate_env_var(name: &str, description: &str) -> Result<(), PersonaManifestError> {
    if !env_var_regex().is_match(name) {
        return Err(PersonaManifestError::InvalidEnvVarName {
            got: name.to_string(),
        });
    }
    if description.trim().is_empty() {
        return Err(PersonaManifestError::EmptyEnvVarDescription {
            name: name.to_string(),
        });
    }
    Ok(())
}

fn validate_contributes(c: &crate::manifest::PersonaContributes) -> Result<(), PersonaManifestError> {
    let lists: [(&'static str, &Vec<String>); 3] = [
        ("agent_configs", &c.agent_configs),
        ("plugin_configs_partial", &c.plugin_configs_partial),
        ("secrets_templates", &c.secrets_templates),
    ];
    for (field, list) in lists {
        for path in list {
            validate_path(field, path)?;
        }
    }
    if let Some(seed) = &c.workspace_seed {
        validate_path("workspace_seed", seed)?;
    }
    Ok(())
}

fn validate_path(field: &'static str, path: &str) -> Result<(), PersonaManifestError> {
    let p = Path::new(path);
    if p.is_absolute() {
        return Err(PersonaManifestError::AbsolutePath {
            field,
            path: path.to_string(),
        });
    }
    // Component-level walk catches `..` even when the path is
    // syntactically relative (`foo/../bar` still escapes).
    for component in p.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(PersonaManifestError::ParentTraversal {
                field,
                path: path.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_str;

    fn minimal_v2() -> &'static str {
        r#"manifest_version = 2

[persona]
id = "cody"
version = "0.2.0"
description = "Programmer pair driving Claude Code goals from chat"
min_nexo_version = ">=0.1.6"
"#
    }

    fn full_v2() -> &'static str {
        r#"manifest_version = 2

[persona]
id = "cody"
version = "0.2.0"
description = "Programmer pair driving Claude Code goals from chat"
min_nexo_version = ">=0.1.6"
homepage = "https://github.com/lordmacu/nexo-persona-cody"

[persona.requires]
plugins = ["telegram", "whatsapp"]
features = ["dispatch_policy:full"]

[[persona.requires.env_vars]]
name = "ANTHROPIC_API_KEY"
required = true
description = "Claude Sonnet 4.6 API key"

[[persona.requires.env_vars]]
name = "TELEGRAM_BOT_TOKEN_CODY"
required = false
description = "Telegram bot token (optional — secrets/ file alternative)"

[persona.contributes]
agent_configs = ["agents.d/cody.yaml"]
plugin_configs_partial = ["plugins/telegram.partial.yaml"]
secrets_templates = ["secrets/cody_token.txt.template"]
workspace_seed = "data/workspace/cody/"

[persona.meta]
author = "Cristian García <informacion@cristiangarcia.co>"
license = "MIT"
repository = "https://github.com/lordmacu/nexo-persona-cody"
"#
    }

    // ── Test 1 ────────────────────────────────────────────
    #[test]
    fn parses_minimal_v2_manifest() {
        let m = parse_str(minimal_v2()).expect("parse");
        assert_eq!(m.manifest_version, 2);
        assert_eq!(m.persona.id, "cody");
        assert_eq!(m.persona.version, "0.2.0");
        assert!(m.persona.requires.is_none());
        assert!(m.persona.contributes.is_none());
        validate(&m).expect("validate");
    }

    // ── Test 2 ────────────────────────────────────────────
    #[test]
    fn parses_full_v2_manifest_with_all_sections() {
        let m = parse_str(full_v2()).expect("parse");
        let req = m.persona.requires.as_ref().expect("requires");
        assert_eq!(req.plugins, vec!["telegram", "whatsapp"]);
        assert_eq!(req.features, vec!["dispatch_policy:full"]);
        assert_eq!(req.env_vars.len(), 2);
        assert!(req.env_vars[0].required);
        assert!(!req.env_vars[1].required);
        let contrib = m.persona.contributes.as_ref().expect("contributes");
        assert_eq!(contrib.agent_configs, vec!["agents.d/cody.yaml"]);
        assert_eq!(contrib.workspace_seed.as_deref(), Some("data/workspace/cody/"));
        let meta = m.persona.meta.as_ref().expect("meta");
        assert_eq!(meta.license.as_deref(), Some("MIT"));
        validate(&m).expect("validate");
    }

    // ── Test 3 ────────────────────────────────────────────
    #[test]
    fn rejects_v1_manifest_with_migration_hint() {
        let v1 = r#"manifest_version = 1
[persona]
id = "cody"
version = "0.1.0"
description = "x"
min_nexo_version = ">=0.1.6"
"#;
        match parse_str(v1) {
            Err(PersonaManifestError::UnsupportedManifestVersion { got, hint }) => {
                assert_eq!(got, 1);
                assert!(hint.contains("install.sh"), "hint must mention install.sh, got: {hint}");
            }
            other => panic!("expected UnsupportedManifestVersion(1), got {other:?}"),
        }
    }

    // ── Test 4 ────────────────────────────────────────────
    #[test]
    fn rejects_unknown_manifest_version() {
        let v3 = r#"manifest_version = 3
[persona]
id = "x"
version = "0.1.0"
description = "x"
min_nexo_version = ">=0.1.0"
"#;
        match parse_str(v3) {
            Err(PersonaManifestError::UnsupportedManifestVersion { got, .. }) => {
                assert_eq!(got, 3);
            }
            other => panic!("expected UnsupportedManifestVersion(3), got {other:?}"),
        }
    }

    // ── Test 5 ────────────────────────────────────────────
    #[test]
    fn rejects_missing_manifest_version() {
        let no_ver = r#"[persona]
id = "x"
version = "0.1.0"
description = "x"
min_nexo_version = ">=0.1.0"
"#;
        match parse_str(no_ver) {
            Err(PersonaManifestError::MissingManifestVersion) => {}
            other => panic!("expected MissingManifestVersion, got {other:?}"),
        }
    }

    // ── Test 6 ────────────────────────────────────────────
    #[test]
    fn rejects_invalid_id_chars() {
        let bad = r#"manifest_version = 2
[persona]
id = "Cody-Bot"
version = "0.1.0"
description = "x"
min_nexo_version = ">=0.1.0"
"#;
        let m = parse_str(bad).expect("typed parse OK");
        match validate(&m) {
            Err(PersonaManifestError::InvalidId { got }) => assert_eq!(got, "Cody-Bot"),
            other => panic!("expected InvalidId, got {other:?}"),
        }
    }

    // ── Test 7 ────────────────────────────────────────────
    #[test]
    fn rejects_invalid_semver_version() {
        let bad = r#"manifest_version = 2
[persona]
id = "cody"
version = "v0.2"
description = "x"
min_nexo_version = ">=0.1.0"
"#;
        let m = parse_str(bad).expect("typed parse OK");
        match validate(&m) {
            Err(PersonaManifestError::InvalidVersion { got, .. }) => assert_eq!(got, "v0.2"),
            other => panic!("expected InvalidVersion, got {other:?}"),
        }
    }

    // ── Test 8 ────────────────────────────────────────────
    #[test]
    fn rejects_invalid_min_nexo_version() {
        let bad = r#"manifest_version = 2
[persona]
id = "cody"
version = "0.2.0"
description = "x"
min_nexo_version = "not-a-req"
"#;
        let m = parse_str(bad).expect("typed parse OK");
        match validate(&m) {
            Err(PersonaManifestError::InvalidMinNexoVersion { got, .. }) => {
                assert_eq!(got, "not-a-req")
            }
            other => panic!("expected InvalidMinNexoVersion, got {other:?}"),
        }
    }

    // ── Test 9 ────────────────────────────────────────────
    #[test]
    fn rejects_empty_description() {
        let bad = r#"manifest_version = 2
[persona]
id = "cody"
version = "0.2.0"
description = "   "
min_nexo_version = ">=0.1.0"
"#;
        let m = parse_str(bad).expect("typed parse OK");
        match validate(&m) {
            Err(PersonaManifestError::EmptyRequiredField { field }) => {
                assert_eq!(field, "description")
            }
            other => panic!("expected EmptyRequiredField(description), got {other:?}"),
        }
    }

    // ── Test 10 ───────────────────────────────────────────
    #[test]
    fn rejects_absolute_contribute_path() {
        let bad = r#"manifest_version = 2
[persona]
id = "cody"
version = "0.2.0"
description = "x"
min_nexo_version = ">=0.1.0"
[persona.contributes]
agent_configs = ["/etc/passwd"]
"#;
        let m = parse_str(bad).expect("typed parse OK");
        match validate(&m) {
            Err(PersonaManifestError::AbsolutePath { field, path }) => {
                assert_eq!(field, "agent_configs");
                assert_eq!(path, "/etc/passwd");
            }
            other => panic!("expected AbsolutePath, got {other:?}"),
        }
    }

    // ── Test 11 ───────────────────────────────────────────
    #[test]
    fn rejects_parent_traversal_contribute_path() {
        let bad = r#"manifest_version = 2
[persona]
id = "cody"
version = "0.2.0"
description = "x"
min_nexo_version = ">=0.1.0"
[persona.contributes]
plugin_configs_partial = ["foo/../../etc/shadow"]
"#;
        let m = parse_str(bad).expect("typed parse OK");
        match validate(&m) {
            Err(PersonaManifestError::ParentTraversal { field, path }) => {
                assert_eq!(field, "plugin_configs_partial");
                assert_eq!(path, "foo/../../etc/shadow");
            }
            other => panic!("expected ParentTraversal, got {other:?}"),
        }
    }

    // ── Test 12 ───────────────────────────────────────────
    #[test]
    fn rejects_invalid_env_var_name_and_empty_description() {
        let bad_name = r#"manifest_version = 2
[persona]
id = "cody"
version = "0.2.0"
description = "x"
min_nexo_version = ">=0.1.0"
[persona.requires]
[[persona.requires.env_vars]]
name = "lowercase_var"
description = "bad name"
"#;
        let m = parse_str(bad_name).expect("typed parse OK");
        match validate(&m) {
            Err(PersonaManifestError::InvalidEnvVarName { got }) => {
                assert_eq!(got, "lowercase_var")
            }
            other => panic!("expected InvalidEnvVarName, got {other:?}"),
        }

        let empty_desc = r#"manifest_version = 2
[persona]
id = "cody"
version = "0.2.0"
description = "x"
min_nexo_version = ">=0.1.0"
[persona.requires]
[[persona.requires.env_vars]]
name = "FOO"
description = ""
"#;
        let m2 = parse_str(empty_desc).expect("typed parse OK");
        match validate(&m2) {
            Err(PersonaManifestError::EmptyEnvVarDescription { name }) => assert_eq!(name, "FOO"),
            other => panic!("expected EmptyEnvVarDescription, got {other:?}"),
        }
    }
}
