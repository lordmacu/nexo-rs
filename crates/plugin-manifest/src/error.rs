//! Typed errors for manifest parse + validation.
//!
//! `ManifestError` encodes failure modes operators encounter when
//! shipping a plugin: invalid TOML, regex-violating ids, semver
//! mismatch, namespace drift, unsafe paths, broken cross-field
//! invariants. Every variant's `Display` impl carries enough
//! context for the operator to fix the manifest without checking
//! source.
//!
//! Validation paths NEVER bail on first error — `PluginManifest::
//! validate(...)` collects every issue into a `Vec<ManifestError>`
//! so the operator fixes everything in one pass.

use crate::manifest::Capability;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("manifest parse error: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("plugin id `{id}` invalid: {reason}")]
    IdInvalid { id: String, reason: &'static str },

    #[error("field `{field}` version `{value}` invalid")]
    VersionInvalid { field: &'static str, value: String },

    #[error(
        "min_nexo_version `{required}` does not match current daemon version `{current}`; \
         upgrade the daemon or downgrade the plugin"
    )]
    MinNexoVersionMismatch { required: String, current: String },

    #[error(
        "tool `{tool_name}` violates namespace policy: must start with `{plugin_id}_`. \
         Rename to `{plugin_id}_<descriptive>`."
    )]
    ToolNamespaceViolation {
        plugin_id: String,
        tool_name: String,
    },

    #[error(
        "path field `{field}` `{path}` rejected: contains `..` (traversal not allowed). \
         Use a path inside the plugin root."
    )]
    PathTraversal { field: &'static str, path: String },

    #[error(
        "path field `{field}` `{path}` rejected: must be relative, not absolute. \
         Paths resolve relative to the plugin root."
    )]
    PathAbsoluteForbidden { field: &'static str, path: String },

    #[error(
        "capability `{capability:?}` declared in `capabilities.provides` but the corresponding \
         section is empty. {hint}"
    )]
    CapabilityWithoutImpl {
        capability: Capability,
        hint: &'static str,
    },

    #[error(
        "tool `{tool_name}` listed in `tools.deferred` but not in `tools.expose`. \
         Deferred tools must be exposed."
    )]
    DeferredNotInExpose { tool_name: String },

    #[error(
        "outbound tool `{tool_name}` in `[[plugin.tools.outbound]]` is not listed in \
         `tools.expose`. Every outbound entry's `name` must also appear in `expose` so the \
         namespace + deferred passes see it. Plugin: `{plugin_id}`."
    )]
    OutboundNotExposed {
        plugin_id: String,
        tool_name: String,
    },

    #[error(
        "outbound tool `{tool_name}` has invalid `input_schema`: {reason}. \
         Plugin: `{plugin_id}`. The schema must be a non-empty JSON object with \
         `\"type\":\"object\"` at the root."
    )]
    OutboundInvalidSchema {
        plugin_id: String,
        tool_name: String,
        reason: String,
    },

    #[error(
        "plugin `{plugin_id}` has invalid `[plugin.config_schema]`: {reason}. \
         The `schema` field must be a non-empty JSON object with \
         `\"type\":\"object\"` at the root."
    )]
    PluginConfigInvalidSchema { plugin_id: String, reason: String },

    #[error("duplicate capability gate env_var `{env_var}` (each gate must be unique)")]
    DuplicateGateEnvVar { env_var: String },

    #[error("invalid channel kind `{kind}`: must match `^[a-z][a-z0-9_]{{0,31}}$`")]
    ChannelKindInvalid { kind: String },

    #[error("plugin name must not be empty")]
    NameEmpty,

    #[error("plugin description must not be empty")]
    DescriptionEmpty,

    #[error(
        "supervisor.stderr_tail_lines `{value}` exceeds cap `{max}`. \
         Lower the value to prevent unbounded ring-buffer memory."
    )]
    SupervisorStderrTailExceedsCap { value: usize, max: usize },

    /// `max_attempts = 0` is silently
    /// equivalent to `respawn = false` (the supervisor publishes
    /// `gave_up` with `attempts: 0` on the very first crash). Use
    /// `respawn = false` for "do not auto-respawn"; reserve
    /// `max_attempts` for the bounded retry count.
    #[error(
        "supervisor.max_attempts must be >= 1 when respawn = true; \
         set respawn = false to disable auto-respawn"
    )]
    SupervisorMaxAttemptsZero,

    /// `backoff_ms = 0` produces a tight
    /// retry loop (the documented exponential schedule starts from
    /// the base; with base = 0 every attempt's wait is 0). Force a
    /// minimum that gives the failing dependency time to recover.
    #[error(
        "supervisor.backoff_ms must be >= {min}; \
         a smaller base produces a tight retry loop \
         that bypasses the documented exponential schedule"
    )]
    SupervisorBackoffMsBelowFloor { value: u64, min: u64 },

    /// `backoff_ms` upper bound. The
    /// reset-counter heuristic (`base * max_attempts * 2`) saturates
    /// at very large bases, effectively disabling the per-window
    /// counter reset. Cap so the heuristic stays meaningful.
    #[error(
        "supervisor.backoff_ms `{value}` exceeds cap `{max}`. \
         A larger base saturates the reset-counter heuristic and \
         disables per-window recovery."
    )]
    SupervisorBackoffMsExceedsCap { value: u64, max: u64 },

    /// Entry in `[plugin.extends].<section>` does
    /// not match the id regex (`^[a-z][a-z0-9_]{0,31}$`).
    #[error("[plugin.extends].{section} id `{id}` invalid: {reason}")]
    ExtendsIdInvalid {
        section: &'static str,
        id: String,
        reason: &'static str,
    },

    /// Same id appears more than once within a
    /// single `[plugin.extends].<section>` list.
    #[error("[plugin.extends].{section} contains duplicate id `{id}`")]
    ExtendsDuplicate { section: &'static str, id: String },

    /// Same id appears in two or more
    /// `[plugin.extends]` lists. Each id must occupy at most one
    /// list within a plugin to keep operator-visible declarations
    /// unambiguous.
    #[error(
        "id `{id}` appears in multiple [plugin.extends] lists ({}); each id must occupy at most one list",
        sections.join(", ")
    )]
    ExtendsCrossListConflict {
        id: String,
        sections: Vec<&'static str>,
    },

    /// Sandbox allowlist entry equals or contains a
    /// host path on the hard denylist. `path` is the manifest
    /// allowlist entry, `denylisted` is the denylist match,
    /// `kind` distinguishes `fs_read_paths` from `fs_write_paths`.
    #[error(
        "[plugin.sandbox].{kind:?} entry `{path}` rejected: equals or covers denylisted host path `{denylisted}`. \
         Narrow the allowlist to a sibling that does not include `{denylisted}`."
    )]
    SandboxAllowlistTouchesDenylist {
        path: String,
        denylisted: String,
        kind: crate::sandbox::SandboxPathKind,
    },

    /// Sandbox allowlist entry must be an absolute
    /// path. Relative paths are ambiguous (relative to plugin
    /// root? cwd? state_dir?) and bwrap binds need absolute
    /// host paths anyway.
    #[error(
        "[plugin.sandbox].{kind:?} entry `{path}` rejected: must be an absolute path starting with `/`."
    )]
    SandboxRelativePath {
        path: String,
        kind: crate::sandbox::SandboxPathKind,
    },

    /// `${state_dir}` token appears in
    /// `fs_read_paths`. State dir is the plugin's per-instance
    /// owned write space; reading from it is meaningless.
    /// Operators using this token are usually trying to declare
    /// a write mount and put it in the wrong list.
    #[error(
        "[plugin.sandbox].fs_read_paths entry `{path}` rejected: `${{state_dir}}` token only allowed in fs_write_paths."
    )]
    SandboxInvalidStateDirInterpolation { path: String },

    /// Manifest declares `network = \"host\"` but
    /// the operator-side capability `NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW`
    /// is not set. Sharing the host network namespace defeats
    /// most of the sandbox; operator must opt in explicitly.
    #[error(
        "[plugin.sandbox].network = \"host\" rejected: requires operator-side capability \
         NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW=1. Either set that env var or switch to network = \"deny\"."
    )]
    SandboxHostNetworkWithoutCapability,

    /// `[plugin.pairing] kind = "form"` declared but the manifest
    /// did not ship any `fields`. Without fields the admin
    /// renders an empty form — almost certainly an authoring
    /// mistake. Phase 81.30 follow-up #5.
    #[error(
        "[plugin.pairing] kind = \"form\" requires at least one entry under [[plugin.pairing.fields]] — empty form would render nothing."
    )]
    PairingFormWithoutFields,

    /// `[plugin.pairing] kind = "custom"` requires the plugin to
    /// declare `rpc_namespace = "..."` so the admin knows which
    /// notify method to subscribe to. Phase 81.30 follow-up #5.
    #[error(
        "[plugin.pairing] kind = \"custom\" requires `rpc_namespace = \"...\"` — admin would not know which `nexo/notify/<rpc_namespace>/status_changed` channel to listen on."
    )]
    PairingCustomWithoutRpcNamespace,

    /// `[plugin.pairing]` declared `fields` but `kind` is not
    /// `form`. Fields are only consumed by the form-flow modal;
    /// any other kind silently ignores them. Reject at boot so
    /// the operator notices the dead config. Phase 81.30 follow-
    /// up #5.
    #[error(
        "[plugin.pairing] fields = [...] only valid with kind = \"form\" (got kind = {kind:?})."
    )]
    PairingFieldsWithoutFormKind {
        /// Kind that was declared instead of `form`.
        kind: String,
    },

    /// `[plugin.pairing.trigger]` shipped a blank `start_method`
    /// or `cancel_method`. Daemon would forward a JSON-RPC to an
    /// empty method name — refuse at boot. Phase 81.20.x Stage 7
    /// Phase 2.
    #[error(
        "[plugin.pairing.trigger].{field} must not be empty — daemon needs a real admin method name to forward to."
    )]
    PairingTriggerEmptyMethod {
        /// Field name that was blank (`start_method` or `cancel_method`).
        field: &'static str,
    },

    /// `[plugin.pairing.trigger]` declared on a non-QR pairing.
    /// Trigger forwarding only makes sense for kinds whose flow
    /// the daemon orchestrates with start/cancel (QR pump). Form
    /// and Info kinds are operator-driven and need no remote
    /// pump. Phase 81.20.x Stage 7 Phase 2.
    #[error(
        "[plugin.pairing.trigger] only valid with kind = \"qr\" (got kind = {kind:?}) — form and info kinds have no remote pump to start/cancel."
    )]
    PairingTriggerOnlyWithQr {
        /// Kind that was declared instead of `qr`.
        kind: String,
    },

    /// `[plugin.public_tunnel]` shipped a blank `close_on_event`
    /// subject. Daemon would subscribe to an empty subject and
    /// either reject (broker validation) or match nothing —
    /// refuse at boot. Phase 81.20.x Stage 7 Phase 2.
    #[error(
        "[plugin.public_tunnel].close_on_event must not be empty — daemon needs a real broker subject to subscribe to."
    )]
    PublicTunnelCloseEventEmpty,

    /// `[plugin.public_tunnel].close_on_event` contained a NATS
    /// wildcard (`*` or `>`). Wildcards would let a stray plugin
    /// event race-close a healthy tunnel; only literal subjects
    /// are accepted. Phase 81.20.x Stage 7 Phase 2.
    #[error(
        "[plugin.public_tunnel].close_on_event = `{subject}` contains a wildcard (`*` or `>`) — only literal broker subjects are accepted."
    )]
    PublicTunnelCloseEventWildcard {
        /// Offending subject.
        subject: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_messages_are_actionable() {
        // Each variant Display includes operator-facing context
        // (the offending value + enough hint to fix). Renderer is
        // implicit via thiserror; this test guards the surface.
        let cases: Vec<Box<dyn std::fmt::Display>> = vec![
            Box::new(ManifestError::IdInvalid {
                id: "Bad-Id".into(),
                reason: "uppercase not allowed",
            }),
            Box::new(ManifestError::VersionInvalid {
                field: "plugin.version",
                value: "abc".into(),
            }),
            Box::new(ManifestError::MinNexoVersionMismatch {
                required: ">=1.0.0".into(),
                current: "0.1.5".into(),
            }),
            Box::new(ManifestError::ToolNamespaceViolation {
                plugin_id: "marketing".into(),
                tool_name: "lead_classify".into(),
            }),
            Box::new(ManifestError::PathTraversal {
                field: "skills.contributes_dir",
                path: "../../etc".into(),
            }),
            Box::new(ManifestError::PathAbsoluteForbidden {
                field: "agents.contributes_dir",
                path: "/etc/secrets".into(),
            }),
            Box::new(ManifestError::DeferredNotInExpose {
                tool_name: "ghost_tool".into(),
            }),
            Box::new(ManifestError::DuplicateGateEnvVar {
                env_var: "MARKETING_API_KEY".into(),
            }),
        ];
        for err in cases {
            let s = err.to_string();
            assert!(!s.is_empty(), "error Display must produce text");
            assert!(s.len() > 20, "error Display must include context: {s:?}");
        }
    }
}
