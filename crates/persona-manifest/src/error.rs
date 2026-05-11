//! Typed errors raised by [`crate::parse_str`] +
//! [`crate::validate`]. Distinct from `nexo-plugin-manifest`'s
//! errors so the CLI can present persona-specific remediation
//! (e.g. "switch to install.sh for v1 packs") without mapping
//! through the plugin error space.

use thiserror::Error;

/// All recoverable failures raised by this crate. Variants are
/// `non_exhaustive` so future schema evolutions don't break
/// downstream `match` users; pattern with a wildcard arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PersonaManifestError {
    /// TOML parse failed before we could even look at fields
    /// (syntax error, unsupported value type, etc.). Carries
    /// the upstream error message so the CLI can echo it.
    #[error("persona manifest TOML parse failed: {0}")]
    ParseToml(String),

    /// `manifest_version` key is absent from the TOML root.
    /// Most likely: file is empty or someone shipped a stub.
    #[error("persona manifest missing required `manifest_version` key")]
    MissingManifestVersion,

    /// `manifest_version` is present but not a value this
    /// daemon recognizes. Hints encode the migration story
    /// (v1 → install.sh, future versions → upgrade).
    #[error("unsupported persona manifest_version `{got}`: {hint}")]
    UnsupportedManifestVersion {
        /// Version the manifest declared.
        got: u32,
        /// Action the operator should take.
        hint: &'static str,
    },

    /// `[persona] id` violates the id regex (lowercase ascii
    /// alphanumerics + dash, 3-64 chars). Mirrors the plugin
    /// id regex so personas + plugins share namespace
    /// conventions.
    #[error("persona id `{got}` violates id regex `^[a-z0-9][a-z0-9-]{{2,63}}$`")]
    InvalidId {
        /// Id string as supplied.
        got: String,
    },

    /// `[persona] version` doesn't parse as a strict
    /// `semver::Version` (`MAJOR.MINOR.PATCH[-pre][+build]`).
    #[error("persona version `{got}` is not strict semver: {reason}")]
    InvalidVersion {
        /// Version string as supplied.
        got: String,
        /// Underlying semver parse error message.
        reason: String,
    },

    /// `[persona] min_nexo_version` doesn't parse as a
    /// `semver::VersionReq` (`>=0.1.6`, `^0.1`, etc.).
    #[error("persona min_nexo_version `{got}` is not a valid semver req: {reason}")]
    InvalidMinNexoVersion {
        /// Version-requirement string as supplied.
        got: String,
        /// Underlying parser error message.
        reason: String,
    },

    /// A `[persona] description` / `id` / `version` /
    /// `min_nexo_version` field is empty after trimming.
    #[error("persona required field `{field}` must not be empty")]
    EmptyRequiredField {
        /// Field name (e.g. `id`, `version`, `description`).
        field: &'static str,
    },

    /// A path under `[persona.contributes]` is not a relative
    /// path (i.e. starts with `/` or a Windows drive letter).
    /// All persona paths must be relative to the extracted
    /// pack root so the installer can place them safely under
    /// the operator's nexo state dir.
    #[error("contributed path `{path}` (in field `{field}`) must be relative; absolute paths are rejected")]
    AbsolutePath {
        /// `agent_configs`, `plugin_configs_partial`, etc.
        field: &'static str,
        /// Offending path string.
        path: String,
    },

    /// A `[persona.contributes]` path contains a parent-
    /// traversal component (`..`). Personas are confined to
    /// their pack root; escaping the root is rejected to
    /// prevent malicious packs writing outside the install
    /// directory.
    #[error("contributed path `{path}` (in field `{field}`) contains `..`; parent-traversal is rejected")]
    ParentTraversal {
        /// `agent_configs`, `plugin_configs_partial`, etc.
        field: &'static str,
        /// Offending path string.
        path: String,
    },

    /// An env-var name in `[[persona.requires.env_vars]]`
    /// violates the POSIX env-var regex (`[A-Z_][A-Z0-9_]*`).
    #[error("env_var name `{got}` violates POSIX env regex `^[A-Z_][A-Z0-9_]*$`")]
    InvalidEnvVarName {
        /// Name as supplied.
        got: String,
    },

    /// An env-var entry has an empty `description`. The CLI
    /// surfaces these strings to the operator at install time;
    /// requiring them keeps the install UX self-explanatory.
    #[error("env_var `{name}` is missing a non-empty `description`")]
    EmptyEnvVarDescription {
        /// Name of the offending env var.
        name: String,
    },
}
