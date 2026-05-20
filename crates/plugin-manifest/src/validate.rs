//! 4-tier defensive validation for `PluginManifest`.
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

// The id regex allows up to 64 chars
// to admit longer multi-segment tool names that legitimate plugins
// use (e.g. `marketing_lead_detect_meeting_intent`, 36 chars).
// `^[a-z][a-z0-9_]{0,63}$` matches the plugin-id regex in
// `id_regex.rs` so `[plugin.extends].tools` accepts every name a
// tool defs author can produce without surprise truncation.
const ID_REGEX_SRC: &str = r"^[a-z][a-z0-9_]{0,63}$";
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

/// Env-aware variant. `host_net_allowed` is the
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
    // extends.tools entries must satisfy the same
    // per-plugin namespace policy as tools.expose.
    validate_tool_namespace(&manifest.plugin.id, &manifest.plugin.extends.tools, errors);
    validate_extends(&manifest.plugin.extends, errors);
    validate_deferred_subset(
        &manifest.plugin.tools.expose,
        &manifest.plugin.tools.deferred,
        errors,
    );
    // Phase 81.33.a — every outbound spec's `name` must appear in
    // expose so the namespace pass + deferred pass observe it.
    // Also checks that the JSON schema string parses as an object.
    validate_outbound_tools(
        &manifest.plugin.id,
        &manifest.plugin.tools.expose,
        &manifest.plugin.tools.outbound,
        errors,
    );
    // Phase 93.1 — declarative plugin config contract. Only the
    // schema *string itself* is validated here; the operator YAML
    // is checked against it at boot (Phase 93.2).
    if let Some(section) = &manifest.plugin.config_schema {
        validate_plugin_config_schema(&manifest.plugin.id, section, errors);
    }
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
    validate_pairing(&manifest.plugin.pairing, errors);
    validate_public_tunnel(&manifest.plugin.public_tunnel, errors);
    validate_admin_ui(manifest, errors);
}

/// Phase 99 Stage 1 — `[plugin.admin_ui]` validator. Delegates to
/// the section's self-contained `validate(plugin_id)` (which
/// collects every contract violation in one pass) and surfaces
/// each as a typed [`ManifestError::AdminUiInvalid`].
fn validate_admin_ui(manifest: &PluginManifest, errors: &mut Vec<ManifestError>) {
    let Some(section) = &manifest.plugin.admin_ui else {
        return;
    };
    for reason in section.validate(&manifest.plugin.id) {
        errors.push(ManifestError::AdminUiInvalid {
            plugin_id: manifest.plugin.id.clone(),
            reason,
        });
    }
}

/// `[plugin.public_tunnel]` validator. `close_on_event` must be
/// a literal broker subject — wildcards would let a stray
/// plugin event race-close a healthy tunnel.
fn validate_public_tunnel(
    section: &crate::public_tunnel::PluginPublicTunnelSection,
    errors: &mut Vec<ManifestError>,
) {
    let Some(subject) = section.close_on_event.as_deref() else {
        return;
    };
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        errors.push(ManifestError::PublicTunnelCloseEventEmpty);
        return;
    }
    if trimmed.split('.').any(|seg| seg == "*" || seg == ">") {
        errors.push(ManifestError::PublicTunnelCloseEventWildcard {
            subject: subject.to_string(),
        });
    }
}

/// Guard against a manifest requesting an
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

    // Supervisor knob bounds. Only enforced
    // when `respawn = true`; if the operator explicitly disabled
    // auto-respawn, max_attempts/backoff_ms are inert and a
    // historical 0 default in the manifest is harmless.
    if supervisor.respawn {
        if supervisor.max_attempts == 0 {
            errors.push(ManifestError::SupervisorMaxAttemptsZero);
        }
        if supervisor.backoff_ms < super::manifest::SUPERVISOR_BACKOFF_MS_MIN {
            errors.push(ManifestError::SupervisorBackoffMsBelowFloor {
                value: supervisor.backoff_ms,
                min: super::manifest::SUPERVISOR_BACKOFF_MS_MIN,
            });
        }
        if supervisor.backoff_ms > super::manifest::SUPERVISOR_BACKOFF_MS_MAX {
            errors.push(ManifestError::SupervisorBackoffMsExceedsCap {
                value: supervisor.backoff_ms,
                max: super::manifest::SUPERVISOR_BACKOFF_MS_MAX,
            });
        }
    }
}

/// Sandbox section validator. Skips entirely when
/// `enabled = false` (the section is descriptive but inactive).
/// Otherwise checks: path absoluteness, denylist match,
/// `${state_dir}` placement, host-network capability gate.
/// Phase 81.30 follow-up #5 — guard against manifest authoring
/// mistakes in `[plugin.pairing]`. Silent dead config (e.g.
/// `kind = "form"` without `fields`) would land in the admin's
/// channel descriptor + render an empty modal at runtime; better
/// to refuse the plugin at boot with a typed error.
fn validate_pairing(pairing: &crate::pairing::PairingSection, errors: &mut Vec<ManifestError>) {
    use crate::pairing::PairingKind;
    let Some(kind) = pairing.kind else {
        return;
    };
    match kind {
        PairingKind::Form => {
            if pairing.fields.is_empty() {
                errors.push(ManifestError::PairingFormWithoutFields);
            }
        }
        PairingKind::Custom => {
            if pairing
                .rpc_namespace
                .as_deref()
                .map(str::is_empty)
                .unwrap_or(true)
            {
                errors.push(ManifestError::PairingCustomWithoutRpcNamespace);
            }
            if !pairing.fields.is_empty() {
                errors.push(ManifestError::PairingFieldsWithoutFormKind {
                    kind: "custom".into(),
                });
            }
        }
        PairingKind::Qr => {
            // Phase 97 — fields are allowed for QR kind so the
            // admin wizard can ask for a discriminator label
            // (e.g. `instance: "ventas"`) BEFORE generating the
            // QR. Without this the operator can't disambiguate
            // multiple WhatsApp accounts and every pair lands
            // on the same default topic. The frontend QrFlow
            // renders declared fields as a form gate that
            // unlocks the "Generar QR" button.
        }
        PairingKind::Info => {
            if !pairing.fields.is_empty() {
                errors.push(ManifestError::PairingFieldsWithoutFormKind {
                    kind: "info".into(),
                });
            }
        }
    }

    if let Some(trigger) = &pairing.trigger {
        if trigger.start_method.trim().is_empty() {
            errors.push(ManifestError::PairingTriggerEmptyMethod {
                field: "start_method",
            });
        }
        if trigger.cancel_method.trim().is_empty() {
            errors.push(ManifestError::PairingTriggerEmptyMethod {
                field: "cancel_method",
            });
        }
        if !matches!(kind, PairingKind::Qr) {
            errors.push(ManifestError::PairingTriggerOnlyWithQr {
                kind: format!("{kind:?}").to_lowercase(),
            });
        }
    }
}

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
        } else if id.len() > 64 {
            "max 64 characters"
        } else {
            "must match ^[a-z][a-z0-9_]{0,63}$"
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
    // Strip any pre-release tag from `current` before matching.
    // The `semver` crate (v1.x) follows Cargo's pre-release rules:
    // a `VersionReq` only matches a pre-release version when at
    // least one comparator explicitly mentions the same
    // (major, minor, patch) tuple. So a daemon built as
    // `0.1.6-rc3` would fail every `min_nexo_version` whose
    // comparator doesn't pin that exact triple — including the
    // catch-all `>=0.0.0`. For `min_nexo_version` the operator
    // intent is purely "daemon is at least X.Y.Z"; release-cycle
    // semantics (rc/alpha/beta) are irrelevant. Strip the tag and
    // match against the bare release version.
    let comparable = if current.pre.is_empty() {
        current.clone()
    } else {
        Version::new(current.major, current.minor, current.patch)
    };
    if !req.matches(&comparable) {
        errors.push(ManifestError::MinNexoVersionMismatch {
            required: req.to_string(),
            current: current.to_string(),
        });
    }
}

/// Validate `[plugin.extends]` lists. Collects
/// every offense (invalid id, within-list dup, cross-list dup)
/// without bailing on the first.
fn validate_extends(extends: &ExtendsSection, errors: &mut Vec<ManifestError>) {
    let regex = id_regex();
    // `tools` joins channels/llm_providers/
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
                    reason: "must match ^[a-z][a-z0-9_]{0,63}$",
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

/// Phase 81.33.a — outbound spec validator.
///
/// Two checks: (1) every `outbound[].name` is in `expose` so the
/// namespace + deferred passes already saw it; (2) `input_schema`
/// is a non-empty JSON value whose top-level type is `"object"`
/// (we don't validate the full schema — only the shape the LLM
/// tool catalog expects).
fn validate_outbound_tools(
    plugin_id: &str,
    expose: &[String],
    outbound: &[crate::manifest::OutboundToolSpec],
    errors: &mut Vec<ManifestError>,
) {
    let expose_set: HashSet<&str> = expose.iter().map(String::as_str).collect();
    for spec in outbound {
        if !expose_set.contains(spec.name.as_str()) {
            errors.push(ManifestError::OutboundNotExposed {
                plugin_id: plugin_id.to_string(),
                tool_name: spec.name.clone(),
            });
        }
        if spec.input_schema.trim().is_empty() {
            errors.push(ManifestError::OutboundInvalidSchema {
                plugin_id: plugin_id.to_string(),
                tool_name: spec.name.clone(),
                reason: "input_schema is empty".to_string(),
            });
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(&spec.input_schema) {
            Ok(serde_json::Value::Object(map)) => {
                // Root must declare `"type":"object"`.
                let ty = map.get("type").and_then(|v| v.as_str());
                if ty != Some("object") {
                    errors.push(ManifestError::OutboundInvalidSchema {
                        plugin_id: plugin_id.to_string(),
                        tool_name: spec.name.clone(),
                        reason: format!(
                            "root `type` must be \"object\", got {:?}",
                            ty.unwrap_or("missing")
                        ),
                    });
                }
            }
            Ok(_) => {
                errors.push(ManifestError::OutboundInvalidSchema {
                    plugin_id: plugin_id.to_string(),
                    tool_name: spec.name.clone(),
                    reason: "input_schema must be a JSON object".to_string(),
                });
            }
            Err(e) => {
                errors.push(ManifestError::OutboundInvalidSchema {
                    plugin_id: plugin_id.to_string(),
                    tool_name: spec.name.clone(),
                    reason: format!("invalid JSON: {e}"),
                });
            }
        }
    }
}

/// Phase 93.1 — declarative `[plugin.config_schema]` validator.
///
/// Same shape as [`validate_outbound_tools`]: collect every
/// failure into `errors` (no short-circuit), emit one
/// [`ManifestError::PluginConfigInvalidSchema`] per problem.
///
/// Checks performed on the schema *string itself* (the operator
/// YAML is validated against it at boot in Phase 93.2):
/// - non-empty after trim,
/// - parses as JSON,
/// - parsed value is a JSON object,
/// - root object declares `"type": "object"` (regardless of
///   `shape` — for `shape = "array"` the schema describes one
///   element, not the wrapper).
fn validate_plugin_config_schema(
    plugin_id: &str,
    section: &crate::manifest::ConfigSchemaSection,
    errors: &mut Vec<ManifestError>,
) {
    if section.schema.trim().is_empty() {
        errors.push(ManifestError::PluginConfigInvalidSchema {
            plugin_id: plugin_id.to_string(),
            reason: "config_schema is empty".to_string(),
        });
        return;
    }
    match serde_json::from_str::<serde_json::Value>(&section.schema) {
        Ok(serde_json::Value::Object(map)) => {
            let ty = map.get("type").and_then(|v| v.as_str());
            if ty != Some("object") {
                errors.push(ManifestError::PluginConfigInvalidSchema {
                    plugin_id: plugin_id.to_string(),
                    reason: format!(
                        "root `type` must be \"object\", got {:?}",
                        ty.unwrap_or("missing")
                    ),
                });
            }
        }
        Ok(_) => {
            errors.push(ManifestError::PluginConfigInvalidSchema {
                plugin_id: plugin_id.to_string(),
                reason: "schema must be a JSON object".to_string(),
            });
        }
        Err(e) => {
            errors.push(ManifestError::PluginConfigInvalidSchema {
                plugin_id: plugin_id.to_string(),
                reason: format!("invalid JSON: {e}"),
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
    if s.split(['/', '\\']).any(|seg| seg == "..") {
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
            // The 4 below are declarative-only for now; their
            // runtime sections land later. We don't fail them
            // here so plugin authors can declare them early.
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
        // 65 chars — one over the new 64-char ceiling bumped in
        // 82.15.bx for multi-segment tool names. 40 chars used
        // to be invalid under the old 32-char ceiling but is now
        // accepted, so the regression target moves with it.
        let long = "a".repeat(65);
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

    /// Phase 81.33.a — outbound spec round-trip parses every
    /// field with default + override values.
    #[test]
    fn manifest_outbound_tools_round_trip() {
        let toml = r#"
[plugin]
id = "telegram"
version = "0.1.0"
name = "Telegram"
description = "Telegram bot"
min_nexo_version = ">=0.1.0"
[plugin.tools]
expose = ["telegram_send_message", "telegram_pin_message"]

[[plugin.tools.outbound]]
name = "telegram_send_message"
description = "Send a Telegram text message."
input_schema = """{"type":"object","properties":{"chat_id":{"type":"string"},"text":{"type":"string"}},"required":["chat_id","text"]}"""

[[plugin.tools.outbound]]
name = "telegram_pin_message"
description = "Pin a message in a chat."
input_schema = """{"type":"object","properties":{"chat_id":{"type":"string"}}}"""
rpc_method = "telegram.pin"
timeout_ms = 5000
"#
        .to_string();
        let m = parse(&toml);
        // Validate is clean — every name is in expose; both
        // schemas parse as valid JSON objects.
        m.validate(&current()).expect("manifest validates");
        let outbound = &m.plugin.tools.outbound;
        assert_eq!(outbound.len(), 2);
        assert_eq!(outbound[0].name, "telegram_send_message");
        assert_eq!(outbound[0].rpc_method, "outbound_tool.invoke");
        assert_eq!(outbound[0].timeout_ms, None);
        assert_eq!(outbound[1].name, "telegram_pin_message");
        assert_eq!(outbound[1].rpc_method, "telegram.pin");
        assert_eq!(outbound[1].timeout_ms, Some(5000));
    }

    /// Phase 81.33.a — outbound entries whose `name` is not in
    /// `expose` must surface a typed error so the operator
    /// fixes the manifest before runtime.
    #[test]
    fn manifest_rejects_outbound_entry_missing_from_expose() {
        let toml = r#"
[plugin]
id = "telegram"
version = "0.1.0"
name = "T"
description = "x"
min_nexo_version = ">=0.1.0"
[plugin.tools]
expose = ["telegram_send_message"]

[[plugin.tools.outbound]]
name = "telegram_ghost"
description = "Not in expose"
input_schema = """{"type":"object"}"""
"#
        .to_string();
        let m = parse(&toml);
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::OutboundNotExposed { plugin_id, tool_name }
                    if plugin_id == "telegram" && tool_name == "telegram_ghost"
            )),
            "expected OutboundNotExposed for telegram_ghost; got {errs:?}"
        );
    }

    /// Phase 81.33.a — invalid `input_schema` strings surface as
    /// `OutboundInvalidSchema` so operators see a clear reason
    /// (empty / non-JSON / wrong shape).
    #[test]
    fn manifest_rejects_outbound_invalid_schema_shapes() {
        let cases: &[(&str, &str)] = &[
            // empty schema
            ("", "empty"),
            // not an object
            (r#""just a string""#, "not an object"),
            // missing type:object
            (r#"{"properties":{}}"#, "missing type"),
            // invalid JSON
            (r#"{not json}"#, "invalid JSON"),
        ];
        for (schema, label) in cases {
            let toml = format!(
                r#"
[plugin]
id = "telegram"
version = "0.1.0"
name = "T"
description = "x"
min_nexo_version = ">=0.1.0"
[plugin.tools]
expose = ["telegram_x"]

[[plugin.tools.outbound]]
name = "telegram_x"
description = "x"
input_schema = '''{schema}'''
"#
            );
            let m = parse(&toml);
            let errs = m
                .validate(&current())
                .expect_err(&format!("schema `{label}` must fail"));
            assert!(
                errs.iter().any(|e| matches!(
                    e,
                    ManifestError::OutboundInvalidSchema { tool_name, .. }
                        if tool_name == "telegram_x"
                )),
                "case `{label}`: expected OutboundInvalidSchema; got {errs:?}"
            );
        }
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

    // ── [plugin.extends] validator ─────────────────────────────

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

    // ── extends.tools validator ───────────────────────────────

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

    // ── sandbox validator ─────────────────────────────────────

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

    // ── supervisor knob bounds ────────────────────────────────

    fn manifest_with_supervisor_block(body: &str) -> PluginManifest {
        let toml = format!("{}\n[plugin.supervisor]\n{}\n", base_manifest_toml(), body);
        parse(&toml)
    }

    #[test]
    fn supervisor_rejects_max_attempts_zero_when_respawn_enabled() {
        // max_attempts = 0 + respawn = true is the silent
        // equivalent of respawn = false (gave_up fires on first
        // crash with attempts: 0). Force the operator to use the
        // explicit `respawn = false` instead.
        let m =
            manifest_with_supervisor_block("respawn = true\nmax_attempts = 0\nbackoff_ms = 1000");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, ManifestError::SupervisorMaxAttemptsZero)),
            "expected SupervisorMaxAttemptsZero, got {errs:?}"
        );
    }

    #[test]
    fn supervisor_rejects_backoff_ms_below_floor_when_respawn_enabled() {
        let m = manifest_with_supervisor_block("respawn = true\nmax_attempts = 3\nbackoff_ms = 50");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::SupervisorBackoffMsBelowFloor {
                    value: 50,
                    min: 100
                }
            )),
            "expected SupervisorBackoffMsBelowFloor, got {errs:?}"
        );
    }

    #[test]
    fn supervisor_rejects_backoff_ms_above_cap_when_respawn_enabled() {
        let m =
            manifest_with_supervisor_block("respawn = true\nmax_attempts = 3\nbackoff_ms = 600000");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::SupervisorBackoffMsExceedsCap {
                    value: 600_000,
                    max: 300_000,
                }
            )),
            "expected SupervisorBackoffMsExceedsCap, got {errs:?}"
        );
    }

    #[test]
    fn supervisor_skips_bounds_when_respawn_disabled() {
        // respawn = false — supervisor knobs are inert. Even an
        // out-of-range backoff_ms / max_attempts = 0 must NOT
        // emit an error so historical manifests with stale values
        // don't break on upgrade.
        let m = manifest_with_supervisor_block(
            "respawn = false\nmax_attempts = 0\nbackoff_ms = 999999",
        );
        let mut errs = Vec::new();
        run_all(&m, &current(), &mut errs);
        let supervisor_errs: Vec<&ManifestError> = errs
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    ManifestError::SupervisorMaxAttemptsZero
                        | ManifestError::SupervisorBackoffMsBelowFloor { .. }
                        | ManifestError::SupervisorBackoffMsExceedsCap { .. }
                )
            })
            .collect();
        assert!(
            supervisor_errs.is_empty(),
            "respawn=false must skip supervisor bounds: got {supervisor_errs:?}"
        );
    }

    #[test]
    fn supervisor_accepts_default_values_with_respawn_enabled() {
        // Default backoff_ms (1000) + max_attempts (3) sit
        // comfortably inside the bounds. Sanity check so a future
        // tightening of the bounds doesn't accidentally break the
        // builtin defaults.
        let m = manifest_with_supervisor_block("respawn = true");
        m.validate(&current()).unwrap();
    }

    // ── Phase 81.30 follow-up #5 — pairing validator ─────────

    fn manifest_with_pairing(body: &str) -> PluginManifest {
        let toml = format!("{}\n[plugin.pairing]\n{}\n", base_manifest_toml(), body);
        parse(&toml)
    }

    #[test]
    fn pairing_form_without_fields_rejected() {
        let m = manifest_with_pairing("kind = \"form\"\nlabel = \"X\"");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, ManifestError::PairingFormWithoutFields)),
            "expected PairingFormWithoutFields, got: {errs:?}"
        );
    }

    #[test]
    fn pairing_form_with_fields_accepted() {
        let m = manifest_with_pairing(
            "kind = \"form\"\nlabel = \"X\"\n\
             [[plugin.pairing.fields]]\n\
             name = \"token\"\n\
             label = \"Bot token\"\n",
        );
        m.validate(&current()).unwrap();
    }

    #[test]
    fn pairing_custom_without_rpc_namespace_rejected() {
        let m = manifest_with_pairing("kind = \"custom\"");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, ManifestError::PairingCustomWithoutRpcNamespace)),
            "expected PairingCustomWithoutRpcNamespace, got: {errs:?}"
        );
    }

    #[test]
    fn pairing_custom_with_rpc_namespace_accepted() {
        let m = manifest_with_pairing("kind = \"custom\"\nrpc_namespace = \"myauth\"");
        m.validate(&current()).unwrap();
    }

    #[test]
    fn pairing_qr_with_fields_accepted() {
        // Phase 97 — QR pairing supports pre-QR form fields so
        // the admin wizard can capture a discriminator label
        // (e.g. WhatsApp `instance: "ventas"`) before generating
        // the QR. The validator MUST allow this shape.
        let m = manifest_with_pairing(
            "kind = \"qr\"\n\
             instance_field = \"instance\"\n\
             [[plugin.pairing.fields]]\n\
             name = \"instance\"\n\
             label = \"Nombre de cuenta\"\n",
        );
        assert!(
            m.validate(&current()).is_ok(),
            "QR pairing should accept pre-QR fields for instance discriminator"
        );
    }

    #[test]
    fn pairing_qr_minimal_accepted() {
        let m = manifest_with_pairing("kind = \"qr\"\nlabel = \"WhatsApp\"");
        m.validate(&current()).unwrap();
    }

    #[test]
    fn pairing_section_absent_skipped() {
        // No `[plugin.pairing]` section at all — validator must
        // pass (the field is opt-in for plugins that don't expose
        // a pair-able channel).
        let m = parse(&base_manifest_toml());
        m.validate(&current()).unwrap();
    }

    // ── Phase 81.20.x Stage 7 Phase 2 — [plugin.pairing.trigger] ───

    #[test]
    fn pairing_trigger_with_qr_accepted() {
        let m = manifest_with_pairing(
            "kind = \"qr\"\nlabel = \"WhatsApp\"\n\
             [plugin.pairing.trigger]\n\
             start_method = \"nexo/admin/whatsapp/pairing/start\"\n\
             cancel_method = \"nexo/admin/whatsapp/pairing/cancel\"\n",
        );
        m.validate(&current()).unwrap();
    }

    #[test]
    fn pairing_trigger_with_form_kind_rejected() {
        let m = manifest_with_pairing(
            "kind = \"form\"\n\
             [[plugin.pairing.fields]]\n\
             name = \"token\"\n\
             label = \"Bot token\"\n\
             [plugin.pairing.trigger]\n\
             start_method = \"a\"\n\
             cancel_method = \"b\"\n",
        );
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, ManifestError::PairingTriggerOnlyWithQr { .. })),
            "expected PairingTriggerOnlyWithQr, got: {errs:?}"
        );
    }

    #[test]
    fn pairing_trigger_blank_start_method_rejected() {
        let m = manifest_with_pairing(
            "kind = \"qr\"\n\
             [plugin.pairing.trigger]\n\
             start_method = \"   \"\n\
             cancel_method = \"nexo/admin/whatsapp/pairing/cancel\"\n",
        );
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::PairingTriggerEmptyMethod { field } if *field == "start_method"
            )),
            "expected PairingTriggerEmptyMethod{{start_method}}, got: {errs:?}"
        );
    }

    #[test]
    fn pairing_trigger_blank_cancel_method_rejected() {
        let m = manifest_with_pairing(
            "kind = \"qr\"\n\
             [plugin.pairing.trigger]\n\
             start_method = \"nexo/admin/whatsapp/pairing/start\"\n\
             cancel_method = \"\"\n",
        );
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::PairingTriggerEmptyMethod { field } if *field == "cancel_method"
            )),
            "expected PairingTriggerEmptyMethod{{cancel_method}}, got: {errs:?}"
        );
    }

    // ── Phase 81.20.x Stage 7 Phase 2 — [plugin.public_tunnel] ───

    fn manifest_with_public_tunnel(body: &str) -> PluginManifest {
        let toml = format!(
            "{}\n[plugin.public_tunnel]\n{}\n",
            base_manifest_toml(),
            body
        );
        parse(&toml)
    }

    #[test]
    fn public_tunnel_enabled_alone_accepted() {
        let m = manifest_with_public_tunnel("enabled = true");
        m.validate(&current()).unwrap();
    }

    #[test]
    fn public_tunnel_with_literal_close_event_accepted() {
        let m = manifest_with_public_tunnel(
            "enabled = true\nclose_on_event = \"plugin.lifecycle.whatsapp.tunnel_done\"",
        );
        m.validate(&current()).unwrap();
    }

    #[test]
    fn public_tunnel_close_event_empty_rejected() {
        let m = manifest_with_public_tunnel("close_on_event = \"   \"");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, ManifestError::PublicTunnelCloseEventEmpty)),
            "expected PublicTunnelCloseEventEmpty, got: {errs:?}"
        );
    }

    #[test]
    fn public_tunnel_close_event_with_wildcard_rejected() {
        let m = manifest_with_public_tunnel("close_on_event = \"plugin.lifecycle.*.tunnel_done\"");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, ManifestError::PublicTunnelCloseEventWildcard { .. })),
            "expected PublicTunnelCloseEventWildcard, got: {errs:?}"
        );
    }

    #[test]
    fn public_tunnel_close_event_with_rest_wildcard_rejected() {
        let m = manifest_with_public_tunnel("close_on_event = \"plugin.lifecycle.whatsapp.>\"");
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, ManifestError::PublicTunnelCloseEventWildcard { .. })),
            "expected PublicTunnelCloseEventWildcard (>), got: {errs:?}"
        );
    }

    // ── Phase 93.1: [plugin.config_schema] ───────────────────────

    fn manifest_with_config_schema(shape: &str, schema: &str) -> String {
        let mut s = base_manifest_toml();
        s.push_str("\n[plugin.config_schema]\n");
        s.push_str(&format!("shape = \"{shape}\"\n"));
        s.push_str(&format!("schema = '''{schema}'''\n"));
        s
    }

    #[test]
    fn config_schema_array_shape_valid() {
        // Telegram-style multi-instance plugin.
        let schema = r#"{"type":"object","properties":{"instance":{"type":"string"},"bot_token_env":{"type":"string"}},"required":["instance","bot_token_env"]}"#;
        let m = parse(&manifest_with_config_schema("array", schema));
        assert_eq!(
            serde_json::to_string(&m.plugin.config_schema.as_ref().unwrap().shape).unwrap(),
            "\"array\"",
            "ConfigShape::Array must serialize as lowercase \"array\"",
        );
        m.validate(&current()).expect("array shape valid");
    }

    #[test]
    fn config_schema_object_shape_valid() {
        // Email-style single-instance plugin.
        let schema = r#"{"type":"object","properties":{"imap_host":{"type":"string"}},"required":["imap_host"]}"#;
        let m = parse(&manifest_with_config_schema("object", schema));
        m.validate(&current()).expect("object shape valid");
    }

    #[test]
    fn config_schema_absent_is_ok() {
        // Manifest without [plugin.config_schema] — backward compat
        // through the Phase 93.5 deprecation window.
        let m = parse(&base_manifest_toml());
        assert!(m.plugin.config_schema.is_none());
        m.validate(&current()).expect("absent section is OK");
    }

    #[test]
    fn config_schema_empty_string_errors() {
        let m = parse(&manifest_with_config_schema("object", ""));
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::PluginConfigInvalidSchema { reason, .. }
                    if reason == "config_schema is empty"
            )),
            "expected empty-schema error, got {errs:?}",
        );
    }

    #[test]
    fn config_schema_root_non_object_errors() {
        let schema = r#"{"type":"string"}"#;
        let m = parse(&manifest_with_config_schema("object", schema));
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::PluginConfigInvalidSchema { reason, .. }
                    if reason.starts_with("root `type` must be \"object\"")
            )),
            "expected root-type error, got {errs:?}",
        );
    }

    #[test]
    fn config_schema_invalid_json_errors() {
        let m = parse(&manifest_with_config_schema("object", "not json"));
        let errs = m.validate(&current()).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ManifestError::PluginConfigInvalidSchema { reason, .. }
                    if reason.starts_with("invalid JSON:")
            )),
            "expected invalid-JSON error, got {errs:?}",
        );
    }

    /// Phase 93.8.a-daemon — `[plugin.credentials_schema]` parses
    /// with both `enabled` and `accounts_shape` fields.
    #[test]
    fn credentials_schema_section_parses_when_present() {
        let mut s = base_manifest_toml();
        s.push_str("\n[plugin.credentials_schema]\n");
        s.push_str("enabled = true\n");
        s.push_str("accounts_shape = \"array\"\n");
        let m = parse(&s);
        let cs = m
            .plugin
            .credentials_schema
            .as_ref()
            .expect("section present");
        assert!(cs.enabled);
        assert_eq!(cs.accounts_shape, Some(crate::manifest::ConfigShape::Array));
    }
}
