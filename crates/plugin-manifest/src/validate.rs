//! Phase 81.1 — 4-tier defensive validation for `PluginManifest`.
//!
//! Validators collect every error (do NOT bail on first) so the
//! operator sees the full diagnostic in one pass. Each validator
//! is independent — one error never blocks another's check.
//!
//! Layers:
//! 1. **Syntactic** — handled by `toml::from_str` +
//!    `#[serde(deny_unknown_fields)]`; not in this module.
//! 2. **Field-level** — id regex, version semver (semver crate
//!    handles parsing), path security.
//! 3. **Cross-field** — capability declared ↔ section populated,
//!    deferred ⊆ expose, tool namespace policy, gate uniqueness.
//! 4. **Runtime** — `min_nexo_version.matches(current)`.

use std::collections::HashSet;
use std::path::Path;

use regex::Regex;
use semver::Version;

use crate::error::ManifestError;
use crate::manifest::{Capability, PluginManifest};

const ID_REGEX_SRC: &str = r"^[a-z][a-z0-9_]{0,31}$";
const CHANNEL_KIND_REGEX_SRC: &str = r"^[a-z][a-z0-9_]{0,31}$";

fn id_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(ID_REGEX_SRC).expect("valid id regex"))
}

fn channel_kind_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(CHANNEL_KIND_REGEX_SRC).expect("valid channel-kind regex"))
}

/// Run every validator and append errors to `errors`.
pub fn run_all(
    manifest: &PluginManifest,
    current_nexo_version: &Version,
    errors: &mut Vec<ManifestError>,
) {
    validate_id(&manifest.plugin.id, errors);
    validate_name(&manifest.plugin.name, errors);
    validate_description(&manifest.plugin.description, errors);
    validate_min_nexo_version(
        &manifest.plugin.min_nexo_version,
        current_nexo_version,
        errors,
    );
    validate_tool_namespace(&manifest.plugin.id, &manifest.plugin.tools.expose, errors);
    validate_deferred_subset(
        &manifest.plugin.tools.expose,
        &manifest.plugin.tools.deferred,
        errors,
    );
    validate_path_security(
        "agents.contributes_dir",
        manifest.plugin.agents.contributes_dir.as_deref(),
        errors,
    );
    validate_path_security(
        "skills.contributes_dir",
        manifest.plugin.skills.contributes_dir.as_deref(),
        errors,
    );
    validate_path_security(
        "config.schema_path",
        manifest.plugin.config.schema_path.as_deref(),
        errors,
    );
    validate_channel_kinds(&manifest.plugin.channels.register, errors);
    validate_capability_impl(manifest, errors);
    validate_capability_gates_unique(&manifest.plugin.capability_gates.gates, errors);
    validate_supervisor(&manifest.plugin.supervisor, errors);
}

/// Phase 81.21.b — guard against a manifest requesting an
/// unbounded stderr tail buffer. The cap is hard-coded in
/// `manifest::SUPERVISOR_STDERR_TAIL_MAX` (today: 512 lines per
/// running plugin) — generous enough for realistic debug needs,
/// small enough to keep the daemon's memory bounded across many
/// plugins.
fn validate_supervisor(
    supervisor: &super::manifest::SupervisorSection,
    errors: &mut Vec<ManifestError>,
) {
    if supervisor.stderr_tail_lines > super::manifest::SUPERVISOR_STDERR_TAIL_MAX {
        errors.push(ManifestError::SupervisorStderrTailExceedsCap {
            value: supervisor.stderr_tail_lines,
            max: super::manifest::SUPERVISOR_STDERR_TAIL_MAX,
        });
    }
}

fn validate_id(id: &str, errors: &mut Vec<ManifestError>) {
    if !id_regex().is_match(id) {
        let reason = if id.is_empty() {
            "must not be empty"
        } else if id.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            "must not start with a digit"
        } else if id.chars().any(|c| c.is_ascii_uppercase()) {
            "uppercase letters not allowed; use lowercase + digits + underscore"
        } else if id.len() > 32 {
            "max 32 characters"
        } else {
            "must match ^[a-z][a-z0-9_]{0,31}$"
        };
        errors.push(ManifestError::IdInvalid {
            id: id.to_string(),
            reason,
        });
    }
}

fn validate_name(name: &str, errors: &mut Vec<ManifestError>) {
    if name.trim().is_empty() {
        errors.push(ManifestError::NameEmpty);
    }
}

fn validate_description(desc: &str, errors: &mut Vec<ManifestError>) {
    if desc.trim().is_empty() {
        errors.push(ManifestError::DescriptionEmpty);
    }
}

fn validate_min_nexo_version(
    req: &semver::VersionReq,
    current: &Version,
    errors: &mut Vec<ManifestError>,
) {
    if !req.matches(current) {
        errors.push(ManifestError::MinNexoVersionMismatch {
            required: req.to_string(),
            current: current.to_string(),
        });
    }
}

fn validate_tool_namespace(
    plugin_id: &str,
    expose: &[String],
    errors: &mut Vec<ManifestError>,
) {
    let prefix = format!("{plugin_id}_");
    for tool_name in expose {
        if !tool_name.starts_with(&prefix) {
            errors.push(ManifestError::ToolNamespaceViolation {
                plugin_id: plugin_id.to_string(),
                tool_name: tool_name.clone(),
            });
        }
    }
}

fn validate_deferred_subset(
    expose: &[String],
    deferred: &[String],
    errors: &mut Vec<ManifestError>,
) {
    let expose_set: HashSet<&str> = expose.iter().map(String::as_str).collect();
    for tool_name in deferred {
        if !expose_set.contains(tool_name.as_str()) {
            errors.push(ManifestError::DeferredNotInExpose {
                tool_name: tool_name.clone(),
            });
        }
    }
}

fn validate_path_security(
    field: &'static str,
    path: Option<&Path>,
    errors: &mut Vec<ManifestError>,
) {
    let Some(p) = path else {
        return;
    };
    let s = p.to_string_lossy();
    // Absolute path check (handles both Unix `/` and Windows
    // drive-letter forms).
    if p.is_absolute() || s.starts_with('/') || s.contains(":\\") {
        errors.push(ManifestError::PathAbsoluteForbidden {
            field,
            path: s.into_owned(),
        });
        return;
    }
    // `..` traversal anywhere in the path.
    if s.split(|c| c == '/' || c == '\\').any(|seg| seg == "..") {
        errors.push(ManifestError::PathTraversal {
            field,
            path: s.into_owned(),
        });
    }
}

fn validate_channel_kinds(
    channels: &[crate::manifest::ChannelDecl],
    errors: &mut Vec<ManifestError>,
) {
    for ch in channels {
        if !channel_kind_regex().is_match(&ch.kind) {
            errors.push(ManifestError::ChannelKindInvalid {
                kind: ch.kind.clone(),
            });
        }
    }
}

fn validate_capability_impl(
    manifest: &PluginManifest,
    errors: &mut Vec<ManifestError>,
) {
    let p = &manifest.plugin;
    for cap in &p.capabilities.provides {
        let (populated, hint) = match cap {
            Capability::Tools => (
                !p.tools.expose.is_empty(),
                "set `[plugin.tools] expose = [...]`",
            ),
            Capability::Advisors => (
                !p.advisors.register.is_empty(),
                "set `[plugin.advisors] register = [...]`",
            ),
            Capability::Agents => (
                p.agents.contributes_dir.is_some(),
                "set `[plugin.agents] contributes_dir = \"...\"`",
            ),
            Capability::Skills => (
                p.skills.contributes_dir.is_some(),
                "set `[plugin.skills] contributes_dir = \"...\"`",
            ),
            Capability::Channels => (
                !p.channels.register.is_empty(),
                "set `[[plugin.channels.register]] kind = \"...\" adapter = \"...\"`",
            ),
            // The 4 below are declarative-only in 81.1; runtime
            // sections defer to later sub-phases. We don't fail
            // them here so plugin authors can declare them early.
            Capability::Hooks
            | Capability::McpServers
            | Capability::Webhooks
            | Capability::PollerDrivers
            | Capability::LlmProviders => (true, ""),
        };
        if !populated {
            errors.push(ManifestError::CapabilityWithoutImpl {
                capability: *cap,
                hint,
            });
        }
    }
}

fn validate_capability_gates_unique(
    gates: &[crate::manifest::CapabilityGateDecl],
    errors: &mut Vec<ManifestError>,
) {
    let mut seen = HashSet::new();
    for g in gates {
        if !seen.insert(&g.env_var) {
            errors.push(ManifestError::DuplicateGateEnvVar {
                env_var: g.env_var.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Capability, PluginManifest};

    fn current() -> Version {
        Version::parse("0.1.0").unwrap()
    }

    fn base_manifest_toml() -> String {
        r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "Marketing"
description = "Lead pipeline"
min_nexo_version = ">=0.1.0"
"#
        .to_string()
    }

    fn parse(toml: &str) -> PluginManifest {
        PluginManifest::from_str(toml).expect("valid TOML")
    }

    #[test]
    fn accepts_valid_minimal_manifest() {
        let m = parse(&base_manifest_toml());
        let res = m.validate(&current());
        assert!(res.is_ok(), "minimal valid manifest must pass: {res:?}");
    }

    #[test]
    fn reject_invalid_id_uppercase() {
        let toml = base_manifest_toml().replace(r#"id = "marketing""#, r#"id = "Marketing""#);
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, ManifestError::IdInvalid { .. })));
    }

    #[test]
    fn reject_invalid_id_starts_with_digit() {
        let toml = base_manifest_toml().replace(r#"id = "marketing""#, r#"id = "1bad""#);
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::IdInvalid { reason, .. } if reason.contains("digit")
            )),
            "got {errs:?}"
        );
    }

    #[test]
    fn reject_invalid_id_too_long() {
        let long = "a".repeat(40);
        let toml = base_manifest_toml().replace(
            r#"id = "marketing""#,
            &format!(r#"id = "{long}""#),
        );
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, ManifestError::IdInvalid { .. })));
    }

    #[test]
    fn reject_min_nexo_version_too_new() {
        let toml = base_manifest_toml()
            .replace(r#"min_nexo_version = ">=0.1.0""#, r#"min_nexo_version = ">=99.0.0""#);
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ManifestError::MinNexoVersionMismatch { .. })));
    }

    #[test]
    fn accept_valid_min_nexo_version() {
        // ">=0.1.0" matches 0.1.0 current.
        let m = parse(&base_manifest_toml());
        m.validate(&current()).unwrap();
    }

    #[test]
    fn reject_tool_namespace_violation() {
        let toml = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "M"
description = "x"
min_nexo_version = ">=0.1.0"
[plugin.tools]
expose = ["lead_classify", "marketing_lead_route"]
"#.to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        let n = errs
            .iter()
            .filter(|e| matches!(e, ManifestError::ToolNamespaceViolation { .. }))
            .count();
        assert_eq!(n, 1, "exactly the unprefixed tool fails: {errs:?}");
    }

    #[test]
    fn reject_deferred_not_in_expose() {
        let toml = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "M"
description = "x"
min_nexo_version = ">=0.1.0"
[plugin.tools]
expose = ["marketing_a"]
deferred = ["marketing_ghost"]
"#.to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ManifestError::DeferredNotInExpose { tool_name } if tool_name == "marketing_ghost"
        )));
    }

    #[test]
    fn reject_path_with_dotdot() {
        let toml = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "M"
description = "x"
min_nexo_version = ">=0.1.0"
[plugin.skills]
contributes_dir = "../etc/secrets"
"#.to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, ManifestError::PathTraversal { .. })));
    }

    #[test]
    fn reject_absolute_path() {
        let toml = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "M"
description = "x"
min_nexo_version = ">=0.1.0"
[plugin.agents]
contributes_dir = "/etc/secrets"
"#.to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ManifestError::PathAbsoluteForbidden { .. })));
    }

    #[test]
    fn reject_capability_without_impl() {
        let toml = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "M"
description = "x"
min_nexo_version = ">=0.1.0"
[plugin.capabilities]
provides = ["agents"]
"#.to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ManifestError::CapabilityWithoutImpl { capability: Capability::Agents, .. }
        )));
    }

    #[test]
    fn reject_invalid_channel_kind() {
        let toml = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "M"
description = "x"
min_nexo_version = ">=0.1.0"
[[plugin.channels.register]]
kind = "BadKind"
adapter = "Foo"
"#.to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, ManifestError::ChannelKindInvalid { .. })));
    }

    #[test]
    fn reject_duplicate_gate_env_var() {
        let toml = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "M"
description = "x"
min_nexo_version = ">=0.1.0"
[[plugin.capability_gates.gate]]
extension = "marketing"
env_var = "DUPE_KEY"
kind = "Boolean"
risk = "Low"
effect = "first"
[[plugin.capability_gates.gate]]
extension = "marketing"
env_var = "DUPE_KEY"
kind = "Boolean"
risk = "Low"
effect = "second"
"#.to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ManifestError::DuplicateGateEnvVar { .. })));
    }

    #[test]
    fn validate_collects_all_errors_not_first() {
        // Multiple violations — validate should report ALL of them.
        let toml = r#"
[plugin]
id = "Bad-Id"
version = "0.1.0"
name = ""
description = "x"
min_nexo_version = ">=99.0.0"
[plugin.tools]
expose = ["wrong_prefix"]
"#.to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        // We expect at least: id invalid, name empty, version mismatch, namespace.
        assert!(errs.len() >= 3, "expected multiple errors, got {errs:?}");
    }
}
