//! Phase 81.1 — typed errors for manifest parse + validation.
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
    IdInvalid {
        id: String,
        reason: &'static str,
    },

    #[error("field `{field}` version `{value}` invalid")]
    VersionInvalid {
        field: &'static str,
        value: String,
    },

    #[error(
        "min_nexo_version `{required}` does not match current daemon version `{current}`; \
         upgrade the daemon or downgrade the plugin"
    )]
    MinNexoVersionMismatch {
        required: String,
        current: String,
    },

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
    PathTraversal {
        field: &'static str,
        path: String,
    },

    #[error(
        "path field `{field}` `{path}` rejected: must be relative, not absolute. \
         Paths resolve relative to the plugin root."
    )]
    PathAbsoluteForbidden {
        field: &'static str,
        path: String,
    },

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
    DeferredNotInExpose {
        tool_name: String,
    },

    #[error("duplicate capability gate env_var `{env_var}` (each gate must be unique)")]
    DuplicateGateEnvVar {
        env_var: String,
    },

    #[error("invalid channel kind `{kind}`: must match `^[a-z][a-z0-9_]{{0,31}}$`")]
    ChannelKindInvalid {
        kind: String,
    },

    #[error("plugin name must not be empty")]
    NameEmpty,

    #[error("plugin description must not be empty")]
    DescriptionEmpty,
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
