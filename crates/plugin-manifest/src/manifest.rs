//! `nexo-plugin.toml` schema definitions + parser.
//!
//! See `lib.rs` module doc for architectural context.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::error::ManifestError;

/// Canonical filename a plugin must ship at its crate root.
pub const PLUGIN_MANIFEST_FILENAME: &str = "nexo-plugin.toml";

// ── Top-level wrapper ────────────────────────────────────────────

/// Top-level TOML structure. Always `[plugin]` table at root.
/// [`PluginManifest::manifest_version`] lets future schema bumps
/// land without an immediate breaking change for plugin authors.
/// Today only `1` (legacy compat mode, auto-translated from the
/// older dual-file shape) and `2` (canonical, this file) are
/// accepted.
///
/// Older plugins omitted the field entirely; the parser maps
/// that to v1 + emits a one-shot deprecation warning. New
/// plugins set `manifest_version = 2` explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// Schema revision of this manifest file. `1` = legacy
    /// (auto-migrated by the parser); `2` = canonical shape.
    /// Defaults to `1` so older manifests (which omitted the
    /// field) keep parsing.
    #[serde(default = "default_manifest_version")]
    pub manifest_version: u32,
    pub plugin: PluginSection,
}

/// Default `manifest_version` when the field is omitted from a
/// plugin's TOML. v1 = legacy mode → triggers compat translation
/// in [`crate::compat_v1`]. New plugins MUST set
/// `manifest_version = 2` to opt into the canonical shape.
fn default_manifest_version() -> u32 {
    1
}

/// Latest manifest schema version this crate understands. Used
/// by `cargo`-driven migrations + JSON-Schema export to lock the
/// canonical contract. Bumping this is a semver-major change.
pub const CURRENT_MANIFEST_VERSION: u32 = 2;

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

    /// Phase 93.1 — declarative config contract.
    ///
    /// When set, the plugin ships a JSON Schema (`schema`) plus
    /// the YAML wire `shape` the daemon should expect at
    /// `cfg.plugins.<plugin_id>`. Absent in manifests written
    /// before 93.1; daemon falls back to typed `cfg.plugins.X`
    /// fields during the deprecation window (closed in Phase 93.5).
    #[serde(default)]
    pub config_schema: Option<ConfigSchemaSection>,

    /// Phase 93.8.a-daemon — optional declarative credential store
    /// contract. When set with `enabled = true`, the daemon's
    /// `SubprocessNexoPlugin::credential_store()` constructs a
    /// `RemoteCredentialStore` and registers it into
    /// `bundle.stores_v2[plugin.id]`. Absent or `enabled = false`
    /// ⇒ no contribution; typed `bundle.stores.X` continues to
    /// serve during the Phase 93.9 deprecation window.
    #[serde(default)]
    pub credentials_schema: Option<CredentialsSchemaSection>,

    /// Per-registry capability declarations.
    /// Subprocess plugins use this to declare which channel
    /// kinds / LLM provider IDs / memory backend IDs / hook IDs
    /// they contribute. Empty section is the legacy default —
    /// in-tree plugins keep working unchanged.
    #[serde(default)]
    pub extends: ExtendsSection,

    #[serde(default)]
    pub requires: RequiresSection,

    #[serde(default)]
    pub capability_gates: CapabilityGatesSection,

    #[serde(default)]
    pub ui: UiSection,

    /// Plugin-driven pairing UI descriptor. Opt-in — plugins
    /// that don't expose a pair-able channel omit the section.
    /// Consumed by the admin `nexo/admin/pairing/channels` RPC
    /// to drive the channel selector + per-channel modal flow.
    #[serde(default, skip_serializing_if = "crate::pairing::PairingSection::is_unset")]
    pub pairing: crate::pairing::PairingSection,

    #[serde(default)]
    pub contracts: ContractsSection,

    #[serde(default)]
    pub meta: MetaSection,

    /// Phase 81.33.b.real Stage 6 — setup wizard dashboard
    /// surface declaration. Plugins describe how their instances
    /// + auth state are discovered. `nexo-setup`'s generic
    /// `ManifestDashboardSource` consumes this section so new
    /// canonical channels (signal, sms, …) auto-surface in the
    /// wizard without an in-tree
    /// `crates/setup/src/services/channels_dashboard.rs` impl.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<crate::dashboard::PluginDashboardSection>,

    /// Phase 81.33.b.real Stage 5 — Prometheus metrics
    /// declaration. Plugins exposing metrics opt in here; the
    /// daemon's `/metrics` aggregator iterates declarers,
    /// issues `<broker_topic_prefix>.metrics.scrape` broker RPC
    /// per scrape, concatenates the returned Prometheus text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<crate::metrics::PluginMetricsSection>,

    /// Phase 81.33.b.real Stage 4 — daemon-forwarded admin RPC
    /// commands the plugin owns. Daemon iterates registered
    /// plugins on every admin call; methods matching a plugin's
    /// `method_prefix` get forwarded via broker JSON-RPC instead
    /// of needing a hardcoded `.with_<plugin>_handle()` builder
    /// call on the dispatcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin: Option<crate::admin::PluginAdminSection>,

    /// Phase 81.33.b.real Stage 2 — daemon-proxied HTTP routes
    /// the plugin owns. Distinct from `[plugin.http_server]`
    /// (which advertises a plugin-bound port the daemon does NOT
    /// proxy): this section declares route prefixes the daemon
    /// mounts on its own HTTP server (`:8080`) and forwards every
    /// request to via broker JSON-RPC.
    ///
    /// Absent / default = no daemon-side proxy. Plugins exposing
    /// pairing pages, OAuth callbacks, webhook endpoints, etc.
    /// declare their mount prefix here so the daemon doesn't need
    /// hardcoded `/whatsapp/*` blocks per plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<crate::http::PluginHttpSection>,

    /// Phase 81.20.x Stage 7 Phase 2 — public-internet tunnel
    /// declaration. When the plugin sets
    /// `[plugin.public_tunnel] enabled = true` AND the operator
    /// sets `NEXO_PLUGIN_PUBLIC_TUNNEL_ALLOW=1`, the daemon
    /// spawns a Cloudflare quick tunnel pointed at its HTTP port
    /// so the plugin's `[plugin.http]` routes become reachable
    /// from outside the LAN. Replaces the deprecated per-plugin
    /// `wa_tunnel_cfg` orchestration with a manifest-driven
    /// contract.
    #[serde(
        default,
        skip_serializing_if = "crate::public_tunnel::PluginPublicTunnelSection::is_unset"
    )]
    pub public_tunnel: crate::public_tunnel::PluginPublicTunnelSection,

    /// Optional subprocess entrypoint. When `command`
    /// is `Some`, the daemon spawns this binary as a child process
    /// and drives the plugin via newline-delimited JSON-RPC over
    /// stdio (`SubprocessNexoPlugin` adapter). When `command` is
    /// `None` (the default), the manifest describes an in-tree
    /// native Rust plugin linked into the daemon binary.
    ///
    /// In-tree plugin manifests (browser / telegram / whatsapp /
    /// email) parse cleanly without this section because every
    /// field defaults.
    #[serde(default)]
    pub entrypoint: EntrypointSection,

    /// Supervisor knobs (auto-respawn config +
    /// stderr tail capture for crashed events). Every field
    /// defaults so manifests without `[plugin.supervisor]` keep
    /// today's behavior (no respawn, 32-line stderr tail).
    #[serde(default)]
    pub supervisor: SupervisorSection,

    /// Bubblewrap-based sandbox manifest section.
    /// Default = disabled, so every existing manifest parses
    /// unchanged. Subprocess plugins opt in via
    /// `[plugin.sandbox] enabled = true` to get fs/network
    /// allowlist enforcement at spawn time on Linux. macOS
    /// is currently a no-op + warn (native sandbox-exec
    /// integration is a future addition).
    #[serde(default)]
    pub sandbox: crate::sandbox::SandboxSection,

    /// Broker subscriptions — NATS subjects the daemon
    /// forwards to this plugin's stdin. Out-of-tree plugins
    /// (marketing, browser) declare their topic prefixes here
    /// instead of negotiating at handshake time. Empty
    /// (default) means "no broker traffic" — purely tool-call
    /// driven plugins keep working unchanged.
    #[serde(default)]
    pub subscriptions: SubscriptionsSection,

    /// Top-level `[plugin.http_server]` — operator-facing
    /// documentation for plugins that bind their own loopback
    /// admin port outside the daemon's spawn supervision
    /// (the marketing extension reads `MARKETING_HTTP_PORT`
    /// directly at boot rather than through a daemon-injected
    /// channel). Distinct from `[plugin.capabilities.http_server]`,
    /// which the daemon DOES consume to supervise spawn-time
    /// bind policy + token allowlisting.
    ///
    /// Accepts an opaque toml table so the field shape stays
    /// permissive — env-driven port (`port_env` + `default_port`)
    /// vs fixed `port` are both legal. Today nothing on the
    /// daemon side reads it; lifting to a typed shape lands
    /// when a second plugin adopts the same pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_server: Option<toml::Value>,
}

// ── Subprocess entrypoint ───────────────────────────────────────

/// Declares how the daemon launches an out-of-tree
/// plugin as a child process. Empty / absent means "this manifest
/// describes an in-tree Rust plugin" — the default, so existing
/// in-tree manifests stay valid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntrypointSection {
    /// Absolute path or PATH-resolvable binary name. `None` =
    /// in-tree plugin (no spawn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Args appended to `command` at spawn time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Environment variables added to the child's environment on
    /// top of the daemon's own `env`. Keys cannot collide with
    /// reserved nexo runtime envs (`NEXO_*`); the spawn helper
    /// rejects collisions before fork.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl EntrypointSection {
    /// `true` when this manifest describes an out-of-tree subprocess
    /// plugin. The daemon's `wire_plugin_registry` reads this to
    /// pick `SubprocessNexoPlugin` factory over the in-tree closure
    /// path. Empty / unset always returns `false` for backward
    /// compatibility with in-tree manifests.
    pub fn is_subprocess(&self) -> bool {
        self.command
            .as_deref()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false)
    }
}

// ── Supervisor ──────────────────────────────────────────────────

/// Supervisor configuration. Controls how the
/// daemon reacts when a subprocess plugin's child process exits
/// unexpectedly.
///
/// Today only `stderr_tail_lines` is read — the host
/// captures that many recent stderr lines and attaches them to
/// the `plugin.lifecycle.<id>.crashed` broker event so operators
/// can debug without grepping logs. Auto-respawn fields parse
/// + validate cleanly but are NOT yet wired (the higher-level
/// supervisor that owns the spawn lifecycle is a future
/// addition).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorSection {
    /// When `true`, the supervisor automatically respawns the
    /// child after a crash, up to `max_attempts` times with
    /// exponential backoff starting at `backoff_ms`. Default
    /// `false` — opt-in because community-tier plugins should
    /// not silently keep restarting if they're broken.
    ///
    /// The manifest field exists today; the actual respawn loop
    /// is a future addition. With this set today, the host
    /// logs a `tracing::info!(target: "plugin.supervisor",
    /// respawn_requested = true, "respawn config not yet wired")`
    /// reminder on first crash so operators know the field is
    /// validated but not yet acted on.
    #[serde(default = "default_supervisor_respawn")]
    pub respawn: bool,

    /// Maximum respawn attempts before the supervisor gives up
    /// and emits a `plugin.lifecycle.<id>.gave_up` event.
    #[serde(default = "default_supervisor_max_attempts")]
    pub max_attempts: u32,

    /// Initial backoff before the first respawn attempt, in
    /// milliseconds. Subsequent attempts double the backoff up
    /// to a hard cap of 60 s.
    #[serde(default = "default_supervisor_backoff_ms")]
    pub backoff_ms: u64,

    /// Number of recent stderr lines to retain in a ring buffer
    /// per running plugin. On crash, the supervisor drains this
    /// buffer into the `stderr_tail` field of the crashed event
    /// payload. Capped at 512 to prevent a manifest from
    /// requesting unbounded memory; values above the cap are
    /// rejected at parse time via [`validate`].
    #[serde(default = "default_supervisor_stderr_tail")]
    pub stderr_tail_lines: usize,
}

fn default_supervisor_respawn() -> bool {
    false
}
fn default_supervisor_max_attempts() -> u32 {
    3
}
fn default_supervisor_backoff_ms() -> u64 {
    1_000
}
fn default_supervisor_stderr_tail() -> usize {
    32
}

impl Default for SupervisorSection {
    fn default() -> Self {
        Self {
            respawn: default_supervisor_respawn(),
            max_attempts: default_supervisor_max_attempts(),
            backoff_ms: default_supervisor_backoff_ms(),
            stderr_tail_lines: default_supervisor_stderr_tail(),
        }
    }
}

/// Hard cap on `stderr_tail_lines`. Prevents a manifest from
/// requesting megabytes of in-memory ring buffer.
pub const SUPERVISOR_STDERR_TAIL_MAX: usize = 512;

/// Minimum allowed `backoff_ms`. A smaller
/// base degenerates the documented exponential schedule into a
/// tight retry loop. 100ms is generous enough for the cheapest
/// healthy plugin and small enough to allow legitimate "fast
/// recovery" tuning.
pub const SUPERVISOR_BACKOFF_MS_MIN: u64 = 100;

/// Maximum allowed `backoff_ms`. Larger bases
/// saturate the `base * max_attempts * 2` reset-counter heuristic
/// (capped internally at 600s) so per-window recovery silently
/// stops working. 5min upper limit keeps the heuristic meaningful
/// for every realistic operator policy.
pub const SUPERVISOR_BACKOFF_MS_MAX: u64 = 300_000;

// ── Capabilities ────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default)]
    pub provides: Vec<Capability>,
    /// Admin RPC capabilities this microapp needs
    /// from the daemon. Layered grant model: required/optional
    /// declared here; operator decides which to grant via
    /// `extensions.yaml.<id>.capabilities_grant`.
    #[serde(default)]
    pub admin: AdminCapabilities,
    /// HTTP server capability. When set, the
    /// microapp serves an HTTP UI / API; the boot supervisor
    /// waits for the health endpoint to return 200 before
    /// marking the extension `ready`. Default `None` keeps
    /// stdio-only microapps unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_server: Option<HttpServerCapability>,
    /// Extension-contributed skill names. Each
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
    /// Extension broker subscriptions /
    /// publish allowlist. Lifts the channel-plugin assumption
    /// that the only broker traffic of interest is
    /// `plugin.outbound.<kind>` (subscribe) +
    /// `plugin.inbound.<kind>` (publish): CRM-style extensions
    /// like marketing flip the directionality (CONSUME
    /// `plugin.inbound.email.>` to ingest leads, PUBLISH
    /// `plugin.outbound.email.>` when the operator approves a
    /// reply). The daemon merges these patterns with the auto-
    /// derived ones at spawn time and rejects publish requests
    /// whose topic fails to match any allowlist entry. Empty /
    /// absent keeps today's auto-derived-only behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker: Option<BrokerCapability>,
}

/// Declarative broker access. Patterns use the
/// NATS-style wildcard grammar (`plugin.inbound.email.>` matches
/// every instance suffix, `*` matches a single segment). Publish
/// patterns are exact-string allowlist entries; the daemon's
/// bridge rejects publish requests whose topic fails to match
/// any allowlist entry via `topic_matches`. Defense-in-depth:
/// a misbehaving plugin can't broadcast on arbitrary topics it
/// didn't declare.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrokerCapability {
    /// Topic patterns the plugin subscribes to. Daemon spawns
    /// one `broker.subscribe` task per pattern at plugin-start
    /// time and pumps `broker.event` frames over stdio.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscribe: Vec<String>,
    /// Topic patterns the plugin is allowed to publish to via
    /// the validated `broker.publish` JSON-RPC notification.
    /// Empty list keeps the default `plugin.inbound.<kind>`
    /// allowlist derived from `[extends.channels]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish: Vec<String>,
}

/// HTTP server declaration.
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
    /// Additional env var names the daemon must let through
    /// the default secret-suffix blocklist when spawning
    /// this microapp. Use case: a microapp that proxies to
    /// a sibling extension (e.g. `agent-creator` →
    /// marketing extension) needs the sibling's bearer
    /// token visible in its own env, but the var name ends
    /// in `_TOKEN` and would otherwise be stripped.
    /// Operators audit this list during install — every
    /// entry is a secret the microapp will see.
    #[serde(default)]
    pub extra_env_passthrough: Vec<String>,
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
        matches!(self.bind.as_str(), "127.0.0.1" | "::1" | "localhost")
    }
}

/// Admin RPC capability declaration.
///
/// Two-tier semantics:
/// - `required`: boot fails if operator did not grant. The
///   microapp cannot run without these (e.g. agent-creator
///   needs `agents_crud` to do its job).
/// - `optional`: boot OK if not granted. Runtime calls to
///   these methods return `-32004 capability_not_granted`. The
///   microapp branches gracefully (e.g. hide an LLM key editor
///   tab when `llm_keys_crud` is missing).
/// Broker subscription declarations — NATS subjects the
/// daemon forwards to the plugin's stdin. Empty by default;
/// plugins that don't subscribe to any broker traffic (purely
/// tool-call driven, e.g. `agent-creator`) leave it unset and
/// keep working unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionsSection {
    /// Subject patterns (`*` glob in the rightmost segment is
    /// honoured by the broker dispatch layer). Examples:
    /// `plugin.inbound.email.*` (every email plugin
    /// instance), `agent.email.notification.*` (notifications
    /// the plugin itself published).
    #[serde(default)]
    pub broker_topics: Vec<String>,
}

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

    /// Phase 81.33.a — full specs for outbound tools the plugin
    /// invokes via JSON-RPC against its subprocess. Each entry's
    /// `name` MUST also appear in `expose` (validated). When
    /// non-empty, the daemon's `register_outbound_tools` call
    /// installs one `GenericRpcToolHandler` per entry — no
    /// daemon-side `nexo_plugin_<id>::register_*_tools(...)`
    /// crate dep needed.
    #[serde(default)]
    pub outbound: Vec<OutboundToolSpec>,
}

/// Phase 81.33.a — per-tool spec for outbound tools registered
/// via manifest + JSON-RPC dispatch. See
/// `spec-runtime-plugin-self-discovery-81.33.a.md` for the
/// full design.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundToolSpec {
    /// Tool name. Must match an entry in
    /// `ToolsSection::expose` and start with `<plugin.id>_`.
    pub name: String,
    /// Description shown to the LLM in the tool catalog.
    pub description: String,
    /// JSON schema for the tool's input parameters. Stored as
    /// a string in the TOML to keep the manifest parser
    /// one-pass; parsed lazily at registration time. The
    /// validator (parse-time) only checks that the string is
    /// non-empty + parses as a JSON value with `"type":"object"`.
    pub input_schema: String,
    /// JSON-RPC method on the subprocess. Defaults to
    /// `"outbound_tool.invoke"` so plugins don't have to
    /// declare it explicitly. Overrideable per entry for
    /// plugins that prefer per-tool methods.
    #[serde(default = "default_outbound_rpc_method")]
    pub rpc_method: String,
    /// Per-call timeout. `None` → daemon-wide default
    /// (`NEXO_PLUGIN_TOOL_TIMEOUT_MS` env, fallback 60_000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

fn default_outbound_rpc_method() -> String {
    "outbound_tool.invoke".to_string()
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

// ── Extends section ─────────────────────────────────────────────

/// Stable section names for `[plugin.extends]`. Ordered to match
/// validator iteration + [`ExtendsSection::all_ids`] output.
pub const EXTENDS_SECTIONS: &[&str] = &[
    "channels",
    "llm_providers",
    "memory_backends",
    "hooks",
    "tools",
];

/// Per-registry capability declaration. Each list names the IDs
/// this plugin contributes to the daemon's corresponding registry
/// slot. Empty section has no runtime effect — the manifest
/// schema simply records intent so out-of-tree plugins declare
/// what they extend.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtendsSection {
    /// Channel kinds (e.g. `["slack", "discord"]`) the plugin's
    /// subprocess implements via the future remote
    /// `ChannelAdapter` wrapper.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<String>,

    /// LLM provider IDs (e.g. `["cohere", "mistral"]`) the
    /// plugin's subprocess implements via the future remote
    /// `LlmClient` wrapper.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub llm_providers: Vec<String>,

    /// Memory backend IDs (e.g. `["pinecone", "qdrant"]`) the
    /// plugin's subprocess implements via the future remote
    /// memory wrapper. One namespace covers
    /// short-term, long-term, and vector tiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_backends: Vec<String>,

    /// HookInterceptor IDs (e.g. `["pii_redact", "rate_limit"]`)
    /// the plugin's subprocess implements via the future remote
    /// hook wrapper.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<String>,

    /// Tool names (e.g. `["browser_navigate",
    /// "browser_click"]`) the plugin's subprocess implements via
    /// the remote `ToolHandler` wrapper. Each tool name MUST also
    /// satisfy the per-plugin namespace rule
    /// (`<plugin_id>_*` or `ext_<plugin_id>_*`); the validator
    /// runs `validate_tool_namespace` against this list as well
    /// as `tools.expose`. Tool schemas (description +
    /// input_schema JSONSchema) are advertised by the subprocess
    /// at handshake via the initialize-reply `tools` array.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
}

impl ExtendsSection {
    /// `true` when all five lists are empty — the manifest
    /// declares no extension contributions.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
            && self.llm_providers.is_empty()
            && self.memory_backends.is_empty()
            && self.hooks.is_empty()
            && self.tools.is_empty()
    }

    /// Flat `(section_name, id)` pairs across all five lists,
    /// in the deterministic order matching [`EXTENDS_SECTIONS`].
    /// Used by the (future) daemon dispatch + doctor surface.
    pub fn all_ids(&self) -> Vec<(&'static str, &str)> {
        let mut out = Vec::with_capacity(
            self.channels.len()
                + self.llm_providers.len()
                + self.memory_backends.len()
                + self.hooks.len()
                + self.tools.len(),
        );
        for id in &self.channels {
            out.push(("channels", id.as_str()));
        }
        for id in &self.llm_providers {
            out.push(("llm_providers", id.as_str()));
        }
        for id in &self.memory_backends {
            out.push(("memory_backends", id.as_str()));
        }
        for id in &self.hooks {
            out.push(("hooks", id.as_str()));
        }
        for id in &self.tools {
            out.push(("tools", id.as_str()));
        }
        out
    }

    /// `true` when `id` appears in the named section. `section`
    /// must match one of [`EXTENDS_SECTIONS`]; unknown sections
    /// return `false`.
    pub fn registers(&self, section: &str, id: &str) -> bool {
        match section {
            "channels" => self.channels.iter().any(|s| s == id),
            "llm_providers" => self.llm_providers.iter().any(|s| s == id),
            "memory_backends" => self.memory_backends.iter().any(|s| s == id),
            "hooks" => self.hooks.iter().any(|s| s == id),
            "tools" => self.tools.iter().any(|s| s == id),
            _ => false,
        }
    }
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

    /// `true` (default) — config changes hot-reload via the
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

// ── Config schema section (Phase 93.1) ──────────────────────────

/// Declarative config contract a plugin ships in its manifest.
///
/// The plugin author embeds a JSON Schema string plus a YAML
/// wire `shape` (`Object` = single map at `cfg.plugins.<id>`,
/// `Array` = `Vec<map>` for multi-instance plugins). The daemon
/// validates the schema string statically (Phase 93.1) and the
/// operator YAML at boot (Phase 93.2). Plugin owns runtime
/// interpretation via `NexoPlugin::configure`.
///
/// The `schema` root MUST declare `"type": "object"` even when
/// `shape = "array"` — the schema describes ONE element, not
/// the wrapper. Phase 93.2 walks the operator YAML and applies
/// the same schema to each element.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSchemaSection {
    /// JSON Schema (draft-07 subset, see
    /// [`crate::config_schema::validate_config`]). Triple-quoted
    /// TOML string. Root must be a non-empty JSON object with
    /// `"type": "object"`.
    pub schema: String,

    /// YAML wire shape at `cfg.plugins.<plugin_id>`.
    pub shape: ConfigShape,

    /// Hot-reload opt-in (mirror [`ConfigSection::hot_reload`]).
    /// Default `true`; plugins set `false` only if config touches
    /// state that requires a restart.
    #[serde(default = "default_true")]
    pub hot_reload: bool,
}

/// YAML wire shape declared by [`ConfigSchemaSection::shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigShape {
    /// Single map at `cfg.plugins.<plugin_id>`.
    Object,
    /// Array of maps at `cfg.plugins.<plugin_id>` — multi-instance plugins.
    Array,
}

// ── Credentials schema section (Phase 93.8.a-daemon) ────────────

/// Phase 93.8.a-daemon — opt-in credential store contribution.
///
/// When `enabled = true`, the subprocess plugin's daemon-side
/// adapter (`SubprocessNexoPlugin::credential_store()`) constructs
/// a `RemoteCredentialStore` and inserts it into
/// `bundle.stores_v2[plugin.id]`. The store forwards each
/// `GenericCredentialStore` method to the matching
/// `plugin.credentials.*` JSON-RPC handler registered on the
/// plugin's `PluginAdapter` (Phase 93.8.a-sdk).
///
/// `accounts_shape` is informational only — daemon doesn't enforce
/// it; reused by future tooling / docs to describe what the
/// operator's YAML at `cfg.plugins.<plugin_id>` looks like.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialsSchemaSection {
    /// `true` ⇒ plugin contributes a credential store via the
    /// Phase 93.8.a RPC contract. `false` / absent ⇒
    /// `NexoPlugin::credential_store()` returns `None`.
    #[serde(default)]
    pub enabled: bool,

    /// YAML wire shape at the operator's `cfg.plugins.<plugin_id>`
    /// account list. Mirrors `ConfigShape` (Phase 93.1) —
    /// `Some(ConfigShape::Array)` for multi-instance plugins
    /// (telegram, whatsapp); `Some(ConfigShape::Object)` for
    /// single-instance (email, browser). `None` ⇒ plugin
    /// describes its own shape.
    #[serde(default)]
    pub accounts_shape: Option<ConfigShape>,
}

// ── contributed-skills validation ────────────────────────────────

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

/// Verify every name in `caps.skills` has a
/// matching `<plugin_root>/skills/<name>/SKILL.md` on disk and
/// passes the slug rule. Returns the list of validated relative
/// paths the daemon's `SkillLoader` should register.
pub fn validate_contributed_skills(
    caps: &Capabilities,
    plugin_root: &std::path::Path,
) -> Result<Vec<PathBuf>, ContributedSkillError> {
    let mut out = Vec::with_capacity(caps.skills.len());
    for name in &caps.skills {
        if !is_valid_skill_slug(name) {
            return Err(ContributedSkillError::InvalidSlug { name: name.clone() });
        }
        let candidate = plugin_root.join("skills").join(name).join("SKILL.md");
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

/// UI hint metadata for a single config field.
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
    /// Parse a manifest from a TOML string. Routes
    /// through [`crate::compat_v1::try_parse_v2_or_v1`] so legacy
    /// (`manifest_version = 1` or absent) docs auto-migrate to
    /// the canonical v2 shape in memory. v1 plugins emit a
    /// one-shot deprecation warning per `(id, version)` tuple.
    ///
    /// Returns SYNTAX/migration errors only; call
    /// [`validate`](Self::validate) for full semantic checks.
    /// Most callers should use [`Self::parse_validated`] which
    /// runs both in one shot.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(toml_src: &str) -> Result<Self, ManifestError> {
        let (parsed, was_v1) = crate::compat_v1::try_parse_v2_or_v1(toml_src)?;
        if was_v1 {
            crate::compat_v1::emit_v1_deprecation_warning(
                &parsed.plugin.id,
                &parsed.plugin.version.to_string(),
            );
        }
        Ok(parsed)
    }

    /// Parse a manifest from a path on disk. Combines IO + parse
    /// errors into [`ManifestError`].
    pub fn from_path(path: &Path) -> Result<Self, ManifestError> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_str(&raw)
    }

    /// Parse + auto-migrate + auto-validate. Most
    /// callers should reach for this so the silent-skip bug in
    /// `admin_capability_collect` (parsing without validating)
    /// goes away framework-wide. Returns the parsed manifest on
    /// success or the full validator error list on failure.
    pub fn parse_validated(
        toml_src: &str,
        current_nexo_version: &Version,
    ) -> Result<Self, Vec<ManifestError>> {
        let parsed = Self::from_str(toml_src).map_err(|e| vec![e])?;
        parsed.validate(current_nexo_version)?;
        Ok(parsed)
    }

    /// Convenience for callers that have a path. Equivalent to
    /// `Self::parse_validated(read_to_string(path)?, …)`.
    pub fn from_path_validated(
        path: &Path,
        current_nexo_version: &Version,
    ) -> Result<Self, Vec<ManifestError>> {
        let raw = std::fs::read_to_string(path).map_err(|e| vec![ManifestError::Io(e)])?;
        Self::parse_validated(&raw, current_nexo_version)
    }

    /// Run the full 4-tier validator. Collects every error
    /// (does NOT bail on first) so the operator fixes everything
    /// in one pass. Empty `Vec` = OK.
    pub fn validate(&self, current_nexo_version: &Version) -> Result<(), Vec<ManifestError>> {
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
        // `subscriptions`
        // defaults to an empty section so legacy manifests
        // (no `[plugin.subscriptions]`) keep parsing.
        assert!(m.plugin.subscriptions.broker_topics.is_empty());
    }

    /// Top-level `[plugin.http_server]` is operator-facing
    /// documentation for self-supervised plugins (marketing
    /// reads `MARKETING_HTTP_PORT` directly). The strict v2
    /// parse must accept the section as an opaque toml::Value
    /// so legacy manifests don't crash boot. Daemon-supervised
    /// HTTP capabilities live under `[plugin.capabilities.http_server]`
    /// (the typed `HttpServerCapability`).
    #[test]
    fn parse_top_level_plugin_http_server_block_accepted_as_opaque() {
        let raw = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "Marketing"
description = "Lead pipeline"
min_nexo_version = ">=0.1.0"

[plugin.http_server]
port_env = "MARKETING_HTTP_PORT"
default_port = 18766
bind = "127.0.0.1"
token_env = "MARKETING_ADMIN_TOKEN"
health_path = "/healthz"
"#;
        let m = PluginManifest::from_str(raw).unwrap();
        let block = m.plugin.http_server.expect("http_server block");
        // Opaque block — caller can introspect when they
        // need to (today nobody does).
        assert_eq!(
            block.get("port_env").and_then(|v| v.as_str()),
            Some("MARKETING_HTTP_PORT"),
        );
        assert_eq!(
            block.get("default_port").and_then(|v| v.as_integer()),
            Some(18766),
        );
    }

    /// Plugins that subscribe to broker traffic declare
    /// `[plugin.subscriptions] broker_topics = [...]`.
    /// Strict v2 parse must accept the section + populate
    /// the topic vec verbatim.
    #[test]
    fn parse_plugin_subscriptions_broker_topics() {
        let raw = r#"
[plugin]
id = "marketing"
version = "0.1.0"
name = "Marketing"
description = "Lead pipeline"
min_nexo_version = ">=0.1.0"

[plugin.subscriptions]
broker_topics = [
    "plugin.inbound.email.*",
    "agent.email.notification.*",
]
"#;
        let m = PluginManifest::from_str(raw).unwrap();
        assert_eq!(
            m.plugin.subscriptions.broker_topics,
            vec![
                "plugin.inbound.email.*".to_string(),
                "agent.email.notification.*".to_string(),
            ]
        );
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
        let m = PluginManifest::from_path(&path).expect("reference manifest must parse");
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

    // ── contributed-skills tests ────────────────────────────────

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
            broker: None,
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
            broker: None,
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
            broker: None,
        };
        let err = validate_contributed_skills(&caps, tmp.path()).unwrap_err();
        assert_eq!(
            err,
            ContributedSkillError::Missing {
                name: "ghost".into()
            }
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
            broker: None,
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
                broker: None,
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
            broker: None,
        };
        let paths = validate_contributed_skills(&caps, tmp.path()).unwrap();
        assert!(paths.is_empty());
    }

    // ── [plugin.extends] section ────────────────────────────────

    #[test]
    fn manifest_extends_section_parses_minimal() {
        let toml_src = r#"
[plugin]
id = "ext_one"
version = "0.1.0"
name = "Ext One"
description = "x"
min_nexo_version = ">=0.1.0"

[plugin.extends]
llm_providers = ["cohere"]
"#;
        let m = PluginManifest::from_str(toml_src).unwrap();
        assert_eq!(m.plugin.extends.llm_providers, vec!["cohere".to_string()]);
        assert!(m.plugin.extends.channels.is_empty());
        assert!(m.plugin.extends.memory_backends.is_empty());
        assert!(m.plugin.extends.hooks.is_empty());
        assert!(!m.plugin.extends.is_empty());
    }

    #[test]
    fn manifest_extends_section_parses_full() {
        let toml_src = r#"
[plugin]
id = "ext_full"
version = "0.1.0"
name = "Ext Full"
description = "x"
min_nexo_version = ">=0.1.0"

[plugin.extends]
channels = ["slack", "discord"]
llm_providers = ["cohere", "mistral"]
memory_backends = ["pinecone"]
hooks = ["pii_redact"]
"#;
        let m = PluginManifest::from_str(toml_src).unwrap();
        assert_eq!(m.plugin.extends.channels.len(), 2);
        assert_eq!(m.plugin.extends.llm_providers.len(), 2);
        assert_eq!(m.plugin.extends.memory_backends.len(), 1);
        assert_eq!(m.plugin.extends.hooks.len(), 1);
        assert!(m.plugin.extends.registers("channels", "slack"));
        assert!(m.plugin.extends.registers("llm_providers", "mistral"));
        assert!(!m.plugin.extends.registers("hooks", "absent"));
        assert!(!m.plugin.extends.registers("unknown_section", "anything"));
        let ids = m.plugin.extends.all_ids();
        assert_eq!(ids.len(), 6);
        // Order: channels first, then llm_providers, then
        // memory_backends, then hooks (matches EXTENDS_SECTIONS).
        assert_eq!(ids[0], ("channels", "slack"));
        assert_eq!(ids[1], ("channels", "discord"));
        assert_eq!(ids[2], ("llm_providers", "cohere"));
        assert_eq!(ids[5], ("hooks", "pii_redact"));
    }

    #[test]
    fn manifest_extends_section_defaults_empty_when_absent() {
        let m = PluginManifest::from_str(minimal_manifest_toml()).unwrap();
        assert!(m.plugin.extends.is_empty());
        assert!(m.plugin.extends.all_ids().is_empty());
    }

    #[test]
    fn manifest_extends_section_serializes_round_trip() {
        let extends = ExtendsSection {
            channels: vec!["slack".into()],
            llm_providers: vec!["cohere".into(), "mistral".into()],
            memory_backends: Vec::new(),
            hooks: vec!["pii_redact".into()],
            tools: Vec::new(),
        };
        let s = toml::to_string(&extends).unwrap();
        // Empty `memory_backends` skipped via skip_serializing_if.
        assert!(!s.contains("memory_backends"));
        assert!(s.contains("channels = [\"slack\"]"));
        assert!(s.contains("hooks = [\"pii_redact\"]"));
        // Round-trip back to ExtendsSection.
        let parsed: ExtendsSection = toml::from_str(&s).unwrap();
        assert_eq!(parsed, extends);
    }

    // ── extends.tools field ─────────────────────────────────────────

    #[test]
    fn manifest_extends_tools_round_trip() {
        let toml_src = r#"
[plugin]
id = "browser"
version = "0.1.0"
name = "Browser"
description = "x"
min_nexo_version = ">=0.1.0"

[plugin.extends]
tools = ["browser_navigate", "browser_click"]
"#;
        let m = PluginManifest::from_str(toml_src).unwrap();
        assert_eq!(m.plugin.extends.tools.len(), 2);
        assert!(m.plugin.extends.registers("tools", "browser_navigate"));
        assert!(!m.plugin.extends.registers("tools", "browser_screenshot"));
    }

    #[test]
    fn manifest_extends_tools_empty_when_absent() {
        let m = PluginManifest::from_str(minimal_manifest_toml()).unwrap();
        assert!(m.plugin.extends.tools.is_empty());
    }

    #[test]
    fn manifest_extends_section_all_ids_includes_tools_in_order() {
        let toml_src = r#"
[plugin]
id = "browser"
version = "0.1.0"
name = "Browser"
description = "x"
min_nexo_version = ">=0.1.0"

[plugin.extends]
channels = ["c1"]
tools = ["browser_t1", "browser_t2"]
"#;
        let m = PluginManifest::from_str(toml_src).unwrap();
        let ids = m.plugin.extends.all_ids();
        assert_eq!(ids[0], ("channels", "c1"));
        assert_eq!(ids[1], ("tools", "browser_t1"));
        assert_eq!(ids[2], ("tools", "browser_t2"));
    }

    #[test]
    fn manifest_extends_section_round_trip_with_tools() {
        let extends = ExtendsSection {
            channels: Vec::new(),
            llm_providers: Vec::new(),
            memory_backends: Vec::new(),
            hooks: Vec::new(),
            tools: vec!["browser_navigate".into(), "browser_click".into()],
        };
        let s = toml::to_string(&extends).unwrap();
        assert!(s.contains("tools = [\"browser_navigate\", \"browser_click\"]"));
        let parsed: ExtendsSection = toml::from_str(&s).unwrap();
        assert_eq!(parsed, extends);
    }
}
