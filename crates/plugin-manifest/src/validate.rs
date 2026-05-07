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

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use regex::Regex;
use semver::Version;

use crate::error::ManifestError;
use crate::manifest::{Capability, ExtendsSection, PluginManifest};
use crate::sandbox::{
    contains_state_dir_token, path_under_or_equals_denylist, SandboxNetwork, SandboxPathKind,
    SandboxSection, SANDBOX_DENYLIST_HOST_PATHS, SANDBOX_STATE_DIR_TOKEN,
};

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

/// Run every validator and append errors to `errors`. Sandbox
/// validation runs with `host_net_allowed = false` (the strict
/// default — operator opt-in is checked host-side at boot via
/// [`run_all_with_sandbox_env`]).
pub fn run_all(
    manifest: &PluginManifest,
    current_nexo_version: &Version,
    errors: &mut Vec<ManifestError>,
) {
    run_all_with_sandbox_env(manifest, current_nexo_version, false, errors);
}

/// Phase 81.22 — env-aware variant. `host_net_allowed` is the
/// boot-time read of `NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW`. The
/// daemon's `wire_plugin_registry` calls this; CLI tools and
/// tests call [`run_all`] which assumes the strict default.
pub fn run_all_with_sandbox_env(
    manifest: &PluginManifest,
    current_nexo_version: &Version,
    host_net_allowed: bool,
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
    // Phase 81.29 — extends.tools entries must satisfy the same
    // per-plugin namespace policy as tools.expose.
    validate_tool_namespace(&manifest.plugin.id, &manifest.plugin.extends.tools, errors);
    validate_extends(&manifest.plugin.extends, errors);
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
    validate_sandbox(&manifest.plugin.sandbox, host_net_allowed, errors);
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

/// Phase 81.22 — sandbox section validator. Skips entirely when
/// `enabled = false` (the section is descriptive but inactive).
/// Otherwise checks: path absoluteness, denylist match,
/// `${state_dir}` placement, host-network capability gate.
fn validate_sandbox(
    sandbox: &SandboxSection,
    host_net_allowed: bool,
    errors: &mut Vec<ManifestError>,
) {
    if !sandbox.enabled {
        return;
    }

    if sandbox.network == SandboxNetwork::Host && !host_net_allowed {
        errors.push(ManifestError::SandboxHostNetworkWithoutCapability);
    }

    for path in &sandbox.fs_read_paths {
        validate_sandbox_path(path, SandboxPathKind::Read, errors);
    }
    for path in &sandbox.fs_write_paths {
        validate_sandbox_path(path, SandboxPathKind::Write, errors);
    }
}

fn validate_sandbox_path(path: &str, kind: SandboxPathKind, errors: &mut Vec<ManifestError>) {
    // ${state_dir} token is only meaningful in fs_write_paths;
    // anywhere else it's a typo for read intent and we surface it
    // explicitly so the operator catches the wrong-list mistake.
    if kind == SandboxPathKind::Read && contains_state_dir_token(path) {
        errors.push(ManifestError::SandboxInvalidStateDirInterpolation {
            path: path.to_string(),
        });
        return;
    }

    // Effective path for absolute-check + denylist: substitute
    // ${state_dir} with a sentinel absolute prefix so the rest of
    // the validator sees a fully-absolute path. Host-side
    // SandboxRunner does the real expansion at spawn time.
    let probe = if contains_state_dir_token(path) {
        path.replace(SANDBOX_STATE_DIR_TOKEN, "/__sandbox_state_dir__")
    } else {
        path.to_string()
    };

    if !probe.starts_with('/') {
        errors.push(ManifestError::SandboxRelativePath {
            path: path.to_string(),
            kind,
        });
        return;
    }

    if let Some(denylisted) = path_under_or_equals_denylist(&probe, SANDBOX_DENYLIST_HOST_PATHS) {
        errors.push(ManifestError::SandboxAllowlistTouchesDenylist {
            path: path.to_string(),
            denylisted: denylisted.to_string(),
            kind,
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

/// Phase 81.28 — validate `[plugin.extends]` lists. Collects
/// every offense (invalid id, within-list dup, cross-list dup)
/// without bailing on the first.
fn validate_extends(extends: &ExtendsSection, errors: &mut Vec<ManifestError>) {
    let regex = id_regex();
    // Phase 81.29 — `tools` joins channels/llm_providers/
    // memory_backends/hooks as the 5th list.
    let lists: [(&'static str, &Vec<String>); 5] = [
        ("channels", &extends.channels),
        ("llm_providers", &extends.llm_providers),
        ("memory_backends", &extends.memory_backends),
        ("hooks", &extends.hooks),
        ("tools", &extends.tools),
    ];

    let mut cross_list: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();

    for (section, list) in &lists {
        let mut seen: HashSet<&str> = HashSet::new();
        for id in list.iter() {
            if !regex.is_match(id) {
                errors.push(ManifestError::ExtendsIdInvalid {
                    section,
                    id: id.clone(),
                    reason: "must match ^[a-z][a-z0-9_]{0,31}$",
                });
                continue;
            }
            if !seen.insert(id.as_str()) {
                errors.push(ManifestError::ExtendsDuplicate {
                    section,
                    id: id.clone(),
                });
                continue;
            }
            cross_list.entry(id.clone()).or_default().push(section);
        }
    }

    // Collapse cross-list duplicates into one error per id with
    // the full list of sections that claimed it.
    for (id, sections) in cross_list {
        if sections.len() > 1 {
            errors.push(ManifestError::ExtendsCrossListConflict { id, sections });
        }
    }
}

fn validate_tool_namespace(plugin_id: &str, expose: &[String], errors: &mut Vec<ManifestError>) {
    let prefix = format!("{plugin_id}_");
    let ext_prefix = format!("ext_{plugin_id}_");
    for tool_name in expose {
        if !tool_name.starts_with(&prefix) && !tool_name.starts_with(&ext_prefix) {
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

fn validate_capability_impl(manifest: &PluginManifest, errors: &mut Vec<ManifestError>) {
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
        assert!(errs
            .iter()
            .any(|e| matches!(e, ManifestError::IdInvalid { .. })));
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
        let toml =
            base_manifest_toml().replace(r#"id = "marketing""#, &format!(r#"id = "{long}""#));
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ManifestError::IdInvalid { .. })));
    }

    #[test]
    fn reject_min_nexo_version_too_new() {
        let toml = base_manifest_toml().replace(
            r#"min_nexo_version = ">=0.1.0""#,
            r#"min_nexo_version = ">=99.0.0""#,
        );
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
"#
        .to_string();
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
"#
        .to_string();
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
"#
        .to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ManifestError::PathTraversal { .. })));
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
"#
        .to_string();
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
"#
        .to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ManifestError::CapabilityWithoutImpl {
                capability: Capability::Agents,
                ..
            }
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
"#
        .to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ManifestError::ChannelKindInvalid { .. })));
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
"#
        .to_string();
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
"#
        .to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        // We expect at least: id invalid, name empty, version mismatch, namespace.
        assert!(errs.len() >= 3, "expected multiple errors, got {errs:?}");
    }

    // ── Phase 81.28 — [plugin.extends] validator ───────────────

    fn manifest_with_extends(extends_block: &str) -> PluginManifest {
        let toml = format!(
            "{}\n[plugin.extends]\n{}\n",
            base_manifest_toml(),
            extends_block
        );
        parse(&toml)
    }

    #[test]
    fn validate_rejects_invalid_extends_id() {
        let m = manifest_with_extends("llm_providers = [\"Cohere\"]");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::ExtendsIdInvalid {
                    section: "llm_providers",
                    id,
                    ..
                } if id == "Cohere"
            )),
            "expected ExtendsIdInvalid, got {errs:?}"
        );
    }

    #[test]
    fn validate_rejects_duplicate_within_list() {
        let m = manifest_with_extends("channels = [\"slack\", \"slack\"]");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::ExtendsDuplicate {
                    section: "channels",
                    id,
                } if id == "slack"
            )),
            "expected ExtendsDuplicate, got {errs:?}"
        );
    }

    #[test]
    fn validate_rejects_cross_list_duplicate() {
        let m = manifest_with_extends("channels = [\"slack\"]\nllm_providers = [\"slack\"]");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::ExtendsCrossListConflict {
                    id,
                    sections,
                } if id == "slack"
                  && sections.contains(&"channels")
                  && sections.contains(&"llm_providers")
            )),
            "expected ExtendsCrossListConflict, got {errs:?}"
        );
    }

    // ── Phase 81.29 — extends.tools validator ─────────────────

    #[test]
    fn validate_extends_tools_rejects_invalid_id() {
        let m = manifest_with_extends("tools = [\"BadTool\"]");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::ExtendsIdInvalid {
                    section: "tools",
                    id,
                    ..
                } if id == "BadTool"
            )),
            "expected ExtendsIdInvalid for tools, got {errs:?}"
        );
    }

    #[test]
    fn validate_extends_tools_rejects_duplicate_within_list() {
        let m = manifest_with_extends("tools = [\"marketing_lead\", \"marketing_lead\"]");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::ExtendsDuplicate {
                    section: "tools",
                    id,
                } if id == "marketing_lead"
            )),
            "expected ExtendsDuplicate for tools, got {errs:?}"
        );
    }

    #[test]
    fn validate_extends_tools_rejects_cross_list_duplicate() {
        let m = manifest_with_extends(
            "channels = [\"marketing_lead\"]\n\
             tools = [\"marketing_lead\"]",
        );
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::ExtendsCrossListConflict {
                    id,
                    sections,
                } if id == "marketing_lead"
                  && sections.contains(&"tools")
                  && sections.contains(&"channels")
            )),
            "expected ExtendsCrossListConflict, got {errs:?}"
        );
    }

    #[test]
    fn validate_extends_tools_must_satisfy_plugin_namespace() {
        let m = manifest_with_extends("tools = [\"foo_bar\"]");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::ToolNamespaceViolation { tool_name, .. }
                if tool_name == "foo_bar"
            )),
            "expected ToolNamespaceViolation for extends.tools entry, got {errs:?}"
        );
    }

    #[test]
    fn extends_section_all_ids_iterator_order_is_deterministic() {
        let m = manifest_with_extends(
            "hooks = [\"h1\"]\n\
             memory_backends = [\"m1\"]\n\
             llm_providers = [\"l1\"]\n\
             channels = [\"c1\"]",
        );
        // Validation passes (all ids unique + valid).
        m.validate(&current()).unwrap();
        // all_ids order matches EXTENDS_SECTIONS regardless of
        // declaration order in the TOML.
        let ids = m.plugin.extends.all_ids();
        assert_eq!(
            ids,
            vec![
                ("channels", "c1"),
                ("llm_providers", "l1"),
                ("memory_backends", "m1"),
                ("hooks", "h1"),
            ]
        );
    }

    // ── Phase 81.22 sandbox validator ─────────────────────────

    fn manifest_with_sandbox(body: &str) -> PluginManifest {
        let toml = format!("{}\n[plugin.sandbox]\n{}\n", base_manifest_toml(), body);
        parse(&toml)
    }

    #[test]
    fn sandbox_disabled_skips_all_checks() {
        // enabled = false: even denylisted paths in the section
        // produce zero violations because the section is inert.
        let m = manifest_with_sandbox(
            "enabled = false\n\
             fs_read_paths = [\"/etc/shadow\"]\n\
             fs_write_paths = [\"relative/path\"]\n\
             network = \"host\"",
        );
        let mut errs = Vec::new();
        run_all_with_sandbox_env(&m, &current(), false, &mut errs);
        assert!(
            !errs.iter().any(|e| matches!(
                e,
                ManifestError::SandboxAllowlistTouchesDenylist { .. }
                    | ManifestError::SandboxRelativePath { .. }
                    | ManifestError::SandboxHostNetworkWithoutCapability
            )),
            "disabled sandbox must not emit sandbox violations: {errs:?}"
        );
    }

    #[test]
    fn sandbox_valid_enabled_section_passes() {
        let m = manifest_with_sandbox(
            "enabled = true\n\
             network = \"deny\"\n\
             fs_read_paths = [\"/etc/ssl/certs\"]\n\
             fs_write_paths = [\"${state_dir}\", \"/tmp/plugin-scratch\"]",
        );
        m.validate(&current()).unwrap();
    }

    #[test]
    fn sandbox_rejects_denylisted_host_path() {
        let m = manifest_with_sandbox(
            "enabled = true\n\
             fs_read_paths = [\"/etc/shadow\"]",
        );
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ManifestError::SandboxAllowlistTouchesDenylist { denylisted, .. }
                if denylisted == "/etc/shadow"
        )));
    }

    #[test]
    fn sandbox_rejects_relative_allowlist_path() {
        let m = manifest_with_sandbox(
            "enabled = true\n\
             fs_write_paths = [\"data/cache\"]",
        );
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ManifestError::SandboxRelativePath { path, kind }
                if path == "data/cache" && *kind == SandboxPathKind::Write
        )));
    }

    #[test]
    fn sandbox_rejects_state_dir_token_in_read_paths() {
        let m = manifest_with_sandbox(
            "enabled = true\n\
             fs_read_paths = [\"${state_dir}/cache\"]",
        );
        let errs = m.validate(&current()).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ManifestError::SandboxInvalidStateDirInterpolation { path }
                if path == "${state_dir}/cache"
        )));
    }

    #[test]
    fn sandbox_rejects_host_network_without_capability() {
        let m = manifest_with_sandbox(
            "enabled = true\n\
             network = \"host\"",
        );
        let mut errs = Vec::new();
        run_all_with_sandbox_env(&m, &current(), false, &mut errs);
        assert!(errs
            .iter()
            .any(|e| matches!(e, ManifestError::SandboxHostNetworkWithoutCapability)));
    }

    #[test]
    fn sandbox_accepts_host_network_with_capability() {
        let m = manifest_with_sandbox(
            "enabled = true\n\
             network = \"host\"",
        );
        let mut errs = Vec::new();
        run_all_with_sandbox_env(&m, &current(), true, &mut errs);
        assert!(!errs
            .iter()
            .any(|e| matches!(e, ManifestError::SandboxHostNetworkWithoutCapability)));
    }
}
