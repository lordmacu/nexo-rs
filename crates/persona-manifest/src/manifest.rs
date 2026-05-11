//! Typed v2 persona-manifest shape — pure deserialization
//! types with `#[deny(unknown_fields)]` so unknown TOML keys
//! surface as parse errors instead of silently dropping (a
//! footgun the operator deserves to know about).
//!
//! The shape mirrors the existing v1 schema (Cody persona pack)
//! field-for-field — the v2 bump signals "consumable by
//! `nexo persona install`", not "different schema". This keeps
//! the migration story trivial: bump `manifest_version` from 1
//! to 2 and the same TOML parses.

use serde::Deserialize;

/// Expected value of `manifest_version` for this crate's
/// schema. v1 is rejected at parse time (handled by
/// [`crate::parse_str`]) — only v2 reaches this typed layer.
pub const MANIFEST_VERSION_V2: u32 = 2;

/// Top-level v2 persona-manifest shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaManifest {
    /// Schema version — must be [`MANIFEST_VERSION_V2`]. The
    /// version check happens in [`crate::parse_str`] before
    /// the typed deserialization, so reaching this struct
    /// implies the version was 2.
    pub manifest_version: u32,
    /// All persona-scoped fields nested under `[persona]`.
    pub persona: PersonaSection,
}

/// Contents of the `[persona]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaSection {
    /// Persona id — must satisfy the id regex enforced by
    /// the validator (lowercase ascii alphanumerics + dash,
    /// 3-64 chars). Globally unique within the operator's
    /// installed personas; collisions are an install-time
    /// error in F3.
    pub id: String,
    /// Strict semver string (`MAJOR.MINOR.PATCH[-pre][+build]`).
    /// Validated by the validator pass.
    pub version: String,
    /// Human-readable one-line description shown by
    /// `nexo persona list` + admin UI.
    pub description: String,
    /// Earliest nexo-rs daemon version this persona is known
    /// to work with (`semver::VersionReq` syntax: `>=0.1.6`,
    /// `^0.1`, etc.). Validated by the validator pass; the
    /// installer compares against the running daemon's
    /// version at install time.
    pub min_nexo_version: String,
    /// Optional homepage URL (operator-facing only — not
    /// validated as a URL here; trust the manifest author).
    #[serde(default)]
    pub homepage: Option<String>,
    /// Plugins / features / env-vars the operator must have
    /// configured. Optional — minimal personas may declare
    /// nothing here.
    #[serde(default)]
    pub requires: Option<PersonaRequires>,
    /// Files the persona ships and where they go. Optional —
    /// some personas may only declare requirements (no files
    /// to lay down).
    #[serde(default)]
    pub contributes: Option<PersonaContributes>,
    /// Author / license / repo metadata. Optional.
    #[serde(default)]
    pub meta: Option<PersonaMeta>,
}

/// Contents of the `[persona.requires]` table.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PersonaRequires {
    /// Plugin ids the operator must have enabled (e.g.
    /// `telegram`, `whatsapp`). Install fails loudly if any
    /// is missing — never auto-installs plugins.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Daemon feature flags the persona depends on (e.g.
    /// `dispatch_policy:full`). Free-form strings; the
    /// installer warns on unknown features but does not
    /// reject (forward-compat).
    #[serde(default)]
    pub features: Vec<String>,
    /// Env vars the operator must export before
    /// `nexo daemon` starts. Each entry's `description`
    /// surfaces in the install-time prompt.
    #[serde(default)]
    pub env_vars: Vec<PersonaEnvVar>,
}

/// One entry under `[[persona.requires.env_vars]]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaEnvVar {
    /// POSIX env-var name (`[A-Z_][A-Z0-9_]*`).
    pub name: String,
    /// Whether the persona refuses to boot without this var.
    /// Defaults to `false` — most env vars are optional fall-
    /// backs for secrets-file-based config.
    #[serde(default)]
    pub required: bool,
    /// One-line description shown to the operator when the
    /// CLI prompts for / lists env vars.
    pub description: String,
}

/// Contents of the `[persona.contributes]` table — the files
/// the persona pack ships, expressed as paths relative to the
/// pack root.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PersonaContributes {
    /// Per-agent YAML files the daemon merges into its
    /// `agents.d/` directory at boot time.
    #[serde(default)]
    pub agent_configs: Vec<String>,
    /// Plugin partial-config snippets merged into the
    /// matching `config/plugins/<name>.yaml` (additive, not
    /// destructive).
    #[serde(default)]
    pub plugin_configs_partial: Vec<String>,
    /// Templates for secrets files the operator must fill in
    /// (e.g. `secrets/foo.txt.template` → operator copies to
    /// `secrets/foo.txt` and edits).
    #[serde(default)]
    pub secrets_templates: Vec<String>,
    /// Optional workspace seed directory copied into the
    /// agent's working directory at first boot.
    #[serde(default)]
    pub workspace_seed: Option<String>,
}

/// Contents of the `[persona.meta]` table — purely
/// informational.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PersonaMeta {
    /// Author string (e.g. `Cristian García <…>`).
    #[serde(default)]
    pub author: Option<String>,
    /// SPDX license identifier (`MIT`, `Apache-2.0`, etc.).
    /// Free-form here; the registry ingestion step (future)
    /// is what enforces SPDX validity.
    #[serde(default)]
    pub license: Option<String>,
    /// Repository URL.
    #[serde(default)]
    pub repository: Option<String>,
}
