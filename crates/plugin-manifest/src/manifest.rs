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
    /// Phase 82.10 — admin RPC capabilities this microapp needs
    /// from the daemon. Layered grant model: required/optional
    /// declared here; operator decides which to grant via
    /// `extensions.yaml.<id>.capabilities_grant`.
    #[serde(default)]
    pub admin: AdminCapabilities,
    /// Phase 82.12 — HTTP server capability. When set, the
    /// microapp serves an HTTP UI / API; the boot supervisor
    /// waits for the health endpoint to return 200 before
    /// marking the extension `ready`. Default `None` keeps
    /// stdio-only microapps unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_server: Option<HttpServerCapability>,
    /// Phase 83.2 — extension-contributed skill names. Each
    /// entry MUST have a matching `<plugin_root>/skills/<name>/SKILL.md`
    /// file (validated by [`validate_contributed_skills`]). The
    /// daemon's skill loader auto-discovers these and merges
    /// them into any agent's skill scope when the extension is
    /// in `agents.yaml.<id>.extensions: [...]`. Operator-
    /// declared `skills_dir` still works; extension skills
    /// merge on top.
    ///
    /// Empty by default; legacy plugins without contributed
    /// skills are unaffected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

/// Phase 82.12 — HTTP server declaration.
///
/// `bind` defaults to `127.0.0.1` (loopback). External bind
/// (`0.0.0.0`, public IP, etc.) requires the operator to flip
/// `extensions.yaml.<id>.allow_external_bind = true` — the
/// boot validator rejects mismatches with
/// `CapabilityBootError::ExternalBindNotAllowed`. Defense in
/// depth against accidentally world-exposed services.
///
/// `token_env` names a process env var the operator populates
/// (e.g. via systemd `EnvironmentFile=`) — the microapp reads
/// it at boot to authenticate inbound HTTP requests against
/// `Authorization: Bearer <token>` or `X-Nexo-Token: <token>`.
/// The daemon also passes it through to the microapp at
/// `initialize` time via the env block; rotation is signalled
/// out-of-band via `nexo/notify/token_rotated`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpServerCapability {
    /// TCP port the microapp will listen on.
    pub port: u16,
    /// Bind address. `127.0.0.1` (default) keeps the server
    /// loopback-only. Anything else requires explicit
    /// per-extension opt-in.
    #[serde(default = "default_http_bind")]
    pub bind: String,
    /// Process env var holding the shared bearer token.
    pub token_env: String,
    /// Health endpoint relative path. Boot supervisor polls
    /// `GET <bind>:<port><health_path>` until 200 (or 30 s
    /// timeout). Default `/healthz`.
    #[serde(default = "default_health_path")]
    pub health_path: String,
}

fn default_http_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_health_path() -> String {
    "/healthz".to_string()
}

impl HttpServerCapability {
    /// `true` when the bind address is loopback. Operators
    /// don't need `allow_external_bind` for these.
    pub fn is_loopback(&self) -> bool {
        matches!(
            self.bind.as_str(),
            "127.0.0.1" | "::1" | "localhost"
        )
    }
}

/// Phase 82.10 — admin RPC capability declaration.
///
/// Two-tier semantics:
/// - `required`: boot fails if operator did not grant. The
///   microapp cannot run without these (e.g. agent-creator
///   needs `agents_crud` to do its job).
/// - `optional`: boot OK if not granted. Runtime calls to
///   these methods return `-32004 capability_not_granted`. The
///   microapp branches gracefully (e.g. hide an LLM key editor
///   tab when `llm_keys_crud` is missing).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdminCapabilities {
    /// Capabilities the microapp cannot run without. Operator
    /// must grant all of these or boot fails with
    /// `CapabilityBootError::RequiredNotGranted`.
    #[serde(default)]
    pub required: Vec<String>,
    /// Capabilities the microapp can run without. Boot warns if
    /// declared but not granted; runtime calls return -32004.
    #[serde(default)]
    pub optional: Vec<String>,
}

impl AdminCapabilities {
    /// All declared capabilities (required ∪ optional).
    pub fn declared(&self) -> std::collections::HashSet<String> {
        self.required
            .iter()
            .chain(self.optional.iter())
            .cloned()
            .collect()
    }
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

// ── Phase 83.2 — contributed-skills validation ───────────────────

/// Errors surfaced when the manifest's declared
/// `[capabilities] skills = [...]` list does not match what's
/// actually on disk under `<plugin_root>/skills/<name>/`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContributedSkillError {
    /// `[capabilities] skills` declares a name with no
    /// `<plugin_root>/skills/<name>/SKILL.md` on disk.
    #[error("contributed skill `{name}` declared in plugin.toml has no `skills/{name}/SKILL.md`")]
    Missing { name: String },
    /// Skill name fails the slug rule (lowercase ASCII, digits,
    /// dashes; max 64 chars). Defends against accidentally
    /// shipping a skill named `../../etc/passwd`.
    #[error("contributed skill name `{name}` is not a valid slug ([a-z0-9-], max 64 chars)")]
    InvalidSlug { name: String },
}

const SKILL_NAME_MAX: usize = 64;

fn is_valid_skill_slug(name: &str) -> bool {
    if name.is_empty() || name.len() > SKILL_NAME_MAX {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Phase 83.2 — verify every name in `caps.skills` has a
/// matching `<plugin_root>/skills/<name>/SKILL.md` on disk and
/// passes the slug rule. Returns the list of validated relative
/// paths the daemon's `SkillLoader` should register; the
/// SkillLoader merge itself is deferred to 83.2.b.
pub fn validate_contributed_skills(
    caps: &Capabilities,
    plugin_root: &std::path::Path,
) -> Result<Vec<PathBuf>, ContributedSkillError> {
    let mut out = Vec::with_capacity(caps.skills.len());
    for name in &caps.skills {
        if !is_valid_skill_slug(name) {
            return Err(ContributedSkillError::InvalidSlug { name: name.clone() });
        }
        let candidate = plugin_root
            .join("skills")
            .join(name)
            .join("SKILL.md");
        if !candidate.is_file() {
            return Err(ContributedSkillError::Missing { name: name.clone() });
        }
        out.push(candidate);
    }
    Ok(out)
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

    #[test]
    fn http_server_defaults_loopback_and_healthz() {
        let toml_src = r#"
            [http_server]
            port = 9001
            token_env = "AGENT_CREATOR_TOKEN"
        "#;
        let parsed: Capabilities = toml::from_str(toml_src).unwrap();
        let http = parsed.http_server.expect("http_server present");
        assert_eq!(http.port, 9001);
        assert_eq!(http.bind, "127.0.0.1");
        assert_eq!(http.health_path, "/healthz");
        assert_eq!(http.token_env, "AGENT_CREATOR_TOKEN");
        assert!(http.is_loopback());
    }

    #[test]
    fn http_server_external_bind_round_trips() {
        let toml_src = r#"
            [http_server]
            port = 8080
            bind = "0.0.0.0"
            token_env = "TOK"
            health_path = "/health"
        "#;
        let parsed: Capabilities = toml::from_str(toml_src).unwrap();
        let http = parsed.http_server.expect("http_server present");
        assert_eq!(http.bind, "0.0.0.0");
        assert!(!http.is_loopback());
        assert_eq!(http.health_path, "/health");
    }

    #[test]
    fn http_server_absent_when_unset() {
        let parsed: Capabilities = toml::from_str("provides = []").unwrap();
        assert!(parsed.http_server.is_none());
    }

    #[test]
    fn http_server_rejects_unknown_field() {
        let toml_src = r#"
            [http_server]
            port = 9001
            token_env = "T"
            extra = true
        "#;
        let err = toml::from_str::<Capabilities>(toml_src).unwrap_err();
        assert!(err.to_string().contains("extra"), "got: {err}");
    }

    // ── Phase 83.2 — contributed-skills tests ───────────────────

    #[test]
    fn contributed_skills_field_round_trips() {
        let toml_src = r#"
            provides = []
            skills = ["ventas-flujo", "soporte-flujo"]
        "#;
        let parsed: Capabilities = toml::from_str(toml_src).unwrap();
        assert_eq!(parsed.skills, vec!["ventas-flujo", "soporte-flujo"]);
    }

    #[test]
    fn contributed_skills_default_empty() {
        let parsed: Capabilities = toml::from_str("provides = []").unwrap();
        assert!(parsed.skills.is_empty());
    }

    #[test]
    fn contributed_skills_skip_serialise_when_empty() {
        let caps = Capabilities {
            provides: Vec::new(),
            admin: AdminCapabilities::default(),
            http_server: None,
            skills: Vec::new(),
        };
        let serialised = toml::to_string(&caps).unwrap();
        assert!(
            !serialised.contains("skills"),
            "empty skills must not appear:\n{serialised}"
        );
    }

    #[test]
    fn validate_contributed_skills_happy_path() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("skills").join("ventas-flujo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# ventas-flujo").unwrap();
        let caps = Capabilities {
            provides: Vec::new(),
            admin: AdminCapabilities::default(),
            http_server: None,
            skills: vec!["ventas-flujo".into()],
        };
        let paths = validate_contributed_skills(&caps, tmp.path()).expect("ok");
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("ventas-flujo/SKILL.md"));
    }

    #[test]
    fn validate_contributed_skills_missing_file_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let caps = Capabilities {
            provides: Vec::new(),
            admin: AdminCapabilities::default(),
            http_server: None,
            skills: vec!["ghost".into()],
        };
        let err = validate_contributed_skills(&caps, tmp.path()).unwrap_err();
        assert_eq!(
            err,
            ContributedSkillError::Missing { name: "ghost".into() }
        );
    }

    #[test]
    fn validate_contributed_skills_rejects_path_traversal_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let caps = Capabilities {
            provides: Vec::new(),
            admin: AdminCapabilities::default(),
            http_server: None,
            // Path-traversal attempt — must be rejected at slug
            // validation BEFORE filesystem access.
            skills: vec!["../../etc/passwd".into()],
        };
        let err = validate_contributed_skills(&caps, tmp.path()).unwrap_err();
        match err {
            ContributedSkillError::InvalidSlug { name } => {
                assert_eq!(name, "../../etc/passwd");
            }
            other => panic!("expected InvalidSlug, got {other:?}"),
        }
    }

    #[test]
    fn validate_contributed_skills_rejects_uppercase_or_special_chars() {
        let tmp = tempfile::tempdir().unwrap();
        for bad in ["VentasFlujo", "ventas_flujo", "ventas flujo", ""] {
            let caps = Capabilities {
                provides: Vec::new(),
                admin: AdminCapabilities::default(),
                http_server: None,
                skills: vec![bad.into()],
            };
            assert!(
                matches!(
                    validate_contributed_skills(&caps, tmp.path()),
                    Err(ContributedSkillError::InvalidSlug { .. })
                ),
                "expected `{bad}` to be rejected as invalid slug"
            );
        }
    }

    #[test]
    fn validate_contributed_skills_empty_list_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let caps = Capabilities {
            provides: Vec::new(),
            admin: AdminCapabilities::default(),
            http_server: None,
            skills: Vec::new(),
        };
        let paths = validate_contributed_skills(&caps, tmp.path()).unwrap();
        assert!(paths.is_empty());
    }
}
