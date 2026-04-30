//! Phase 81.1 — `nexo-plugin.toml` schema definitions + parser.
//!
//! See `lib.rs` module doc for IRROMPIBLE refs and architectural
//! context.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::error::ManifestError;

/// Canonical filename a plugin must ship at its crate root.
pub const PLUGIN_MANIFEST_FILENAME: &str = "nexo-plugin.toml";

// ── Top-level wrapper ────────────────────────────────────────────

/// Top-level TOML structure. Always `[plugin]` table at root.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub plugin: PluginSection,
}

// ── Main plugin section ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSection {
    /// Plugin identifier. Regex `^[a-z][a-z0-9_]{0,31}$`.
    /// Used as the prefix every plugin-shipped tool name must
    /// start with (namespace policy, validated in 81.3).
    pub id: String,

    /// Plugin version (semver). Required.
    pub version: Version,

    /// Human-readable plugin name (UI / docs).
    pub name: String,

    /// One-line description.
    pub description: String,

    /// Minimum nexo daemon version required. The boot loader
    /// rejects manifests whose `min_nexo_version` does not match
    /// the running daemon's `CARGO_PKG_VERSION`.
    ///
    /// Example: `min_nexo_version = ">=0.1.0"` admits 0.1.x and
    /// later; `^0.1.0` admits 0.1.x only (semver caret).
    pub min_nexo_version: VersionReq,

    /// `true` opts the plugin in at boot without explicit
    /// operator action. Default `false` for safety —
    /// operator must explicitly enable plugins (defense-in-depth).
    #[serde(default)]
    pub enabled_by_default: bool,

    #[serde(default)]
    pub capabilities: Capabilities,

    #[serde(default)]
    pub tools: ToolsSection,

    #[serde(default)]
    pub advisors: AdvisorsSection,

    #[serde(default)]
    pub agents: AgentsSection,

    #[serde(default)]
    pub channels: ChannelsSection,

    #[serde(default)]
    pub skills: SkillsSection,

    #[serde(default)]
    pub config: ConfigSection,

    #[serde(default)]
    pub requires: RequiresSection,

    #[serde(default)]
    pub capability_gates: CapabilityGatesSection,

    #[serde(default)]
    pub ui: UiSection,

    #[serde(default)]
    pub contracts: ContractsSection,

    #[serde(default)]
    pub meta: MetaSection,
}

// ── Capabilities ────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default)]
    pub provides: Vec<Capability>,
}

/// Closed enum of what a plugin can declaratively contribute.
/// Adding a variant is a semver-major bump of the manifest
/// schema (deserialization is strict via `deny_unknown_fields`
/// at parent level + serde's enum match).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Tools,
    Advisors,
    Agents,
    Skills,
    Channels,
    Hooks,
    McpServers,
    Webhooks,
    PollerDrivers,
    LlmProviders,
}

// ── Tools section ───────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    /// Tool names this plugin registers. Each MUST start with
    /// `<plugin.id>_` (validated). Empty if plugin doesn't
    /// register any tools (e.g. ships only advisors or skills).
    #[serde(default)]
    pub expose: Vec<String>,

    /// Subset of `expose` to mark `ToolMeta::deferred()`. Plugin
    /// authors mark verbose-schema tools deferred so they only
    /// surface via `ToolSearch`.
    #[serde(default)]
    pub deferred: Vec<String>,
}

// ── Advisors section ────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisorsSection {
    /// Names of `ToolAdvisor` impls the plugin registers.
    /// Mainly used for documentation + diagnostics — runtime
    /// registration happens via `NexoPlugin::init` (81.2).
    #[serde(default)]
    pub register: Vec<String>,
}

// ── Agents section ──────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsSection {
    /// Relative path inside the plugin root containing
    /// `agents.d/*.yaml` files merged into the runtime config.
    /// Default: no contribution.
    #[serde(default)]
    pub contributes_dir: Option<PathBuf>,

    /// When `true`, plugin agents may override existing agents
    /// with the same `id`. Default `false` — collisions are
    /// rejected.
    #[serde(default)]
    pub allow_override: bool,
}

// ── Channels section ────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelsSection {
    /// New channel kinds this plugin registers via the
    /// `ChannelAdapter` trait (81.8).
    #[serde(default)]
    pub register: Vec<ChannelDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelDecl {
    /// Channel kind identifier. Globally unique. Regex
    /// `^[a-z][a-z0-9_]{0,31}$`.
    pub kind: String,
    /// Implementation type name (informational).
    pub adapter: String,
}

// ── Skills section ──────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsSection {
    /// Relative path inside the plugin root containing
    /// `<skill_name>/SKILL.md` files. Registered with
    /// `SkillLoader` so any agent can declare them in its
    /// `skills` list.
    #[serde(default)]
    pub contributes_dir: Option<PathBuf>,
}

// ── Config section ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSection {
    /// Optional JSONSchema for the plugin's own config blocks.
    /// Loader validates `config/plugins/<id>/*.yaml` against
    /// this schema (defer to 81.4).
    #[serde(default)]
    pub schema_path: Option<PathBuf>,

    /// `true` (default) — config changes hot-reload via Phase 18
    /// file watcher. Plugin opts out if its config touches
    /// global state that requires restart.
    #[serde(default = "default_true")]
    pub hot_reload: bool,
}

impl Default for ConfigSection {
    fn default() -> Self {
        Self {
            schema_path: None,
            hot_reload: true,
        }
    }
}

fn default_true() -> bool {
    true
}

// ── Requires section ────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiresSection {
    /// External binaries the plugin invokes via `std::process`.
    /// Boot doctor warns on missing.
    #[serde(default)]
    pub bins: Vec<String>,

    /// Required environment variables. Boot doctor warns on
    /// missing; plugin may skip activation.
    #[serde(default)]
    pub env: Vec<String>,

    /// Required Cargo features. Informational — Cargo build
    /// already enforces.
    #[serde(default)]
    pub features: Vec<String>,

    /// Framework capabilities required (e.g. `"broker"`,
    /// `"memory"`, `"long_term_memory"`). Boot validator confirms
    /// they are wired before activating the plugin.
    #[serde(default)]
    pub nexo_capabilities: Vec<String>,
}

// ── Capability gates section ────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGatesSection {
    /// Capability gates this plugin contributes to the
    /// `nexo-setup::capabilities::INVENTORY`.
    #[serde(default, rename = "gate")]
    pub gates: Vec<CapabilityGateDecl>,
}

/// Mirror of `nexo_setup::CapabilityToggle` shape (kept as a
/// wire-shape struct here to avoid a `nexo-plugin-manifest →
/// nexo-setup` dep that would be hard to maintain — `nexo-setup`
/// is the consumer).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGateDecl {
    /// Plugin extension id this gate belongs to.
    pub extension: String,
    /// Env var operator sets to enable the gate.
    pub env_var: String,
    pub kind: GateKind,
    pub risk: GateRisk,
    /// Plain-English description of what enabling does.
    pub effect: String,
    /// Optional shell hint operators paste verbatim.
    #[serde(default)]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum GateKind {
    Boolean,
    CargoFeature,
    Allowlist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum GateRisk {
    Low,
    Medium,
    High,
    Critical,
}

// ── UI section ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSection {
    /// Per-config-field admin UI hints. Keyed by dotted-path
    /// field name (e.g. `"marketing.api_key"`).
    #[serde(default)]
    pub fields: BTreeMap<String, UiHint>,
}

/// Mirror of OpenClaw's `PluginConfigUiHint` (research/src/
/// plugins/manifest-types.ts:1-8).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiHint {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub advanced: bool,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
}

// ── Contracts section ───────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractsSection {
    /// Contract identifiers this plugin provides. Other plugins
    /// can declare them in `consumes` to express dependencies.
    /// (Topological sort + cycle detection deferred to 81.5.)
    #[serde(default)]
    pub provides: Vec<String>,

    /// Contract identifiers this plugin requires.
    #[serde(default)]
    pub consumes: Vec<String>,
}

// ── Meta section ────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaSection {
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
}

// ── Public API ──────────────────────────────────────────────────

impl PluginManifest {
    /// Parse a manifest from a TOML string. Returns syntactic
    /// errors only; call [`validate`](Self::validate) for full
    /// semantic checks.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(toml_src: &str) -> Result<Self, ManifestError> {
        Ok(toml::from_str(toml_src)?)
    }

    /// Parse a manifest from a path on disk. Combines IO + parse
    /// errors into [`ManifestError`].
    pub fn from_path(path: &Path) -> Result<Self, ManifestError> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_str(&raw)
    }

    /// Run the full 4-tier validator. Collects every error
    /// (does NOT bail on first) so the operator fixes everything
    /// in one pass. Empty `Vec` = OK.
    pub fn validate(
        &self,
        current_nexo_version: &Version,
    ) -> Result<(), Vec<ManifestError>> {
        let mut errors = Vec::new();
        crate::validate::run_all(self, current_nexo_version, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn id(&self) -> &str {
        &self.plugin.id
    }

    pub fn version(&self) -> &Version {
        &self.plugin.version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_manifest_toml() -> &'static str {
        r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "Marketing"
description = "Lead pipeline plugin"
min_nexo_version = ">=0.1.0"
"#
    }

    #[test]
    fn parse_minimal_valid_manifest() {
        let m = PluginManifest::from_str(minimal_manifest_toml()).unwrap();
        assert_eq!(m.id(), "marketing");
        assert_eq!(m.version().to_string(), "0.1.0");
        assert!(!m.plugin.enabled_by_default, "default opt-in is false");
    }

    #[test]
    fn parse_full_manifest_round_trip() {
        let toml_src = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "Marketing"
description = "Lead pipeline"
min_nexo_version = ">=0.1.0"
enabled_by_default = false

[plugin.capabilities]
provides = ["tools", "advisors"]

[plugin.tools]
expose = ["marketing_lead_classify", "marketing_lead_route"]
deferred = ["marketing_lead_classify"]

[plugin.advisors]
register = ["MarketingAdvisor"]

[plugin.requires]
env = ["MARKETING_API_KEY"]
"#;
        let m = PluginManifest::from_str(toml_src).unwrap();
        assert_eq!(m.plugin.tools.expose.len(), 2);
        assert_eq!(m.plugin.tools.deferred.len(), 1);
        assert_eq!(m.plugin.capabilities.provides.len(), 2);
        assert_eq!(m.plugin.requires.env, vec!["MARKETING_API_KEY".to_string()]);
    }

    #[test]
    fn reject_unknown_field() {
        let toml_src = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "x"
description = "x"
min_nexo_version = ">=0.1.0"
something_unknown = true
"#;
        let result = PluginManifest::from_str(toml_src);
        assert!(result.is_err(), "unknown field must be rejected");
    }

    #[test]
    fn reject_missing_required_id() {
        let toml_src = r#"
[plugin]
version = "0.1.0"
name = "x"
description = "x"
min_nexo_version = ">=0.1.0"
"#;
        assert!(PluginManifest::from_str(toml_src).is_err());
    }

    #[test]
    fn reject_missing_required_version() {
        let toml_src = r#"
[plugin]
id = "marketing"
name = "x"
description = "x"
min_nexo_version = ">=0.1.0"
"#;
        assert!(PluginManifest::from_str(toml_src).is_err());
    }

    #[test]
    fn reject_invalid_version_string() {
        let toml_src = r#"
[plugin]
id = "marketing"
version = "not.a.semver"
name = "x"
description = "x"
min_nexo_version = ">=0.1.0"
"#;
        assert!(PluginManifest::from_str(toml_src).is_err());
    }

    #[test]
    fn default_enabled_by_default_false() {
        let m = PluginManifest::from_str(minimal_manifest_toml()).unwrap();
        assert!(!m.plugin.enabled_by_default);
    }

    #[test]
    fn default_config_hot_reload_true() {
        let m = PluginManifest::from_str(minimal_manifest_toml()).unwrap();
        assert!(m.plugin.config.hot_reload, "hot_reload defaults true");
    }

    #[test]
    fn example_marketing_manifest_validates() {
        // Reference manifest at examples/marketing-example.toml
        // must parse + validate cleanly. Catches drift if a
        // schema change breaks the canonical example.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples")
            .join("marketing-example.toml");
        let m = PluginManifest::from_path(&path)
            .expect("reference manifest must parse");
        let current = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        m.validate(&current)
            .unwrap_or_else(|errs| panic!("reference manifest must validate: {errs:?}"));
    }

    #[test]
    fn capability_serde_round_trip_via_toml() {
        // Verify each Capability variant round-trips through the
        // canonical snake_case wire shape used by manifests. We
        // wrap each variant in a temporary `Capabilities` struct
        // and round-trip via TOML (which is the actual transport
        // for `nexo-plugin.toml`).
        let cases = [
            (Capability::Tools, "tools"),
            (Capability::Advisors, "advisors"),
            (Capability::Agents, "agents"),
            (Capability::Skills, "skills"),
            (Capability::Channels, "channels"),
            (Capability::Hooks, "hooks"),
            (Capability::McpServers, "mcp_servers"),
            (Capability::Webhooks, "webhooks"),
            (Capability::PollerDrivers, "poller_drivers"),
            (Capability::LlmProviders, "llm_providers"),
        ];
        for (variant, snake) in cases {
            let toml_src = format!("provides = [\"{snake}\"]\n");
            let parsed: Capabilities = toml::from_str(&toml_src).unwrap();
            assert_eq!(parsed.provides, vec![variant], "round-trip {variant:?}");
        }
    }
}
