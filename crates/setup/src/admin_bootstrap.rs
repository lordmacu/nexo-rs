//! Admin RPC bootstrap.
//!
//! Builds the daemon-side glue between the admin RPC layer
//! (`nexo_core::agent::admin_rpc::*`) and the extension host
//! (`nexo_extensions::runtime::*`). Owned by `nexo-setup` so
//! `nexo-core` stays free of `nexo-extensions` dep direction
//! that would form a cycle.
//!
//! Boot wires a single [`AdminRpcBootstrap`] for the daemon.
//! The spawn loop calls [`spawn_options_for`] once per
//! extension to obtain a `StdioSpawnOptions` pre-populated
//! with the per-microapp `admin_router`; post-spawn it calls
//! [`bind_writer`] with the runtime's `outbox_sender()` so
//! admin response frames flow back to the live stdin.
//!
//! Runtime task: [`spawn_prune_task`] kicks a 30 s loop that
//! pages stale entries out of the in-memory pairing challenge
//! store. The handle returned by [`AdminRpcBootstrap::build`]
//! aborts the loop on drop.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use nexo_config::types::extensions::ExtensionsConfig;
use nexo_core::agent::admin_rpc::dispatcher::ReloadSignal;
use nexo_core::agent::admin_rpc::domains::agent_events::TranscriptReader;
use nexo_core::agent::admin_rpc::domains::pairing::PairingChallengeStore;
use nexo_core::agent::admin_rpc::{
    validate_capabilities_at_boot, AdminAuditWriter, AdminCapabilityDecl, AdminRpcDispatcher,
    CapabilitySet, DispatcherAdminRouter, InMemoryAuditWriter, SqliteAdminAuditWriter,
};
use nexo_core::agent::agent_events::{
    AgentEventEmitter, BroadcastAgentEventEmitter, NoopAgentEventEmitter,
};
use nexo_extensions::runtime::admin_router::SharedAdminRouter;
use nexo_extensions::runtime::stdio::StdioSpawnOptions;
use nexo_plugin_manifest::manifest::{AdminCapabilities, HttpServerCapability};
use nexo_tool_meta::admin::agent_events::{AgentEventKind, AGENT_EVENT_NOTIFY_METHOD};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::admin_adapters::{
    json_rpc_notification, AgentsYamlPatcher, DeferredAdminOutboundWriter,
    FilesystemCredentialStore, InMemoryPairingChallengeStore, LlmYamlPatcherFs,
};
use nexo_core::agent::admin_rpc::domains::processing::ProcessingControlStore;

/// Capability that lets a microapp receive `TranscriptAppended`
/// events.
const CAP_TRANSCRIPTS_SUBSCRIBE: &str = "transcripts_subscribe";
/// Capability that lets a microapp receive every emitted kind.
const CAP_AGENT_EVENTS_SUBSCRIBE_ALL: &str = "agent_events_subscribe_all";
/// INVENTORY env var that hard-disables the firehose + backfill
/// subsystem regardless of grants.
const ENV_AGENT_EVENTS_ENABLED: &str = "NEXO_MICROAPP_AGENT_EVENTS_ENABLED";

fn agent_events_enabled() -> bool {
    match std::env::var(ENV_AGENT_EVENTS_ENABLED) {
        Ok(v) => !matches!(v.trim(), "0" | "false" | "FALSE" | "off" | "OFF" | ""),
        Err(_) => true,
    }
}

/// Per-domain admin RPC kill switches.
///
/// Maps each operator-grantable capability to its
/// `NEXO_MICROAPP_ADMIN_<DOMAIN>_ENABLED` env var. When the env
/// var is set to `0` / `false` / `off` (anything explicitly
/// "off"-ish), the capability is stripped from every microapp's
/// grant set. Capabilities whose env var is unset OR set to a
/// truthy value pass through unchanged.
///
/// Mirrors the INVENTORY entries in `capabilities.rs` so the
/// operator-visible `agent doctor capabilities` reading and the
/// runtime gating stay in sync.
const ADMIN_DOMAIN_KILL_SWITCHES: &[(&str, &str)] = &[
    ("agents_crud", "NEXO_MICROAPP_ADMIN_AGENTS_ENABLED"),
    (
        "credentials_crud",
        "NEXO_MICROAPP_ADMIN_CREDENTIALS_ENABLED",
    ),
    ("pairing_initiate", "NEXO_MICROAPP_ADMIN_PAIRING_ENABLED"),
    ("llm_keys_crud", "NEXO_MICROAPP_ADMIN_LLM_KEYS_ENABLED"),
    ("channels_crud", "NEXO_MICROAPP_ADMIN_CHANNELS_ENABLED"),
    ("skills_crud", "NEXO_MICROAPP_ADMIN_SKILLS_ENABLED"),
    ("tenants_crud", "NEXO_MICROAPP_ADMIN_TENANTS_ENABLED"),
    ("secrets_write", "NEXO_MICROAPP_ADMIN_SECRETS_ENABLED"),
    ("auth_rotate", "NEXO_MICROAPP_ADMIN_AUTH_ENABLED"),
];

fn is_off_value(v: &str) -> bool {
    matches!(v.trim(), "0" | "false" | "FALSE" | "off" | "OFF")
}

/// Strip from `grants` every capability whose
/// [`ADMIN_DOMAIN_KILL_SWITCHES`] env var resolves (via
/// `env_get`) to an "off" value. Returns `(microapp_id,
/// capability, env_var)` tuples documenting each strip so the
/// caller can emit operator-visible warnings.
///
/// Unset env vars (returning `None`) are treated as "ON" — the
/// documented default behaviour matches the INVENTORY
/// description ("Default ON (no env var set behaves as `1`)").
fn apply_admin_domain_kill_switches<F>(
    grants: &mut std::collections::HashMap<String, std::collections::HashSet<String>>,
    env_get: F,
) -> Vec<(String, String, &'static str)>
where
    F: Fn(&str) -> Option<String>,
{
    let mut killed: Vec<&'static str> = Vec::new();
    for (cap, env_var) in ADMIN_DOMAIN_KILL_SWITCHES {
        if let Some(v) = env_get(env_var) {
            if is_off_value(&v) {
                killed.push(cap);
            }
        }
    }
    if killed.is_empty() {
        return Vec::new();
    }
    let mut stripped: Vec<(String, String, &'static str)> = Vec::new();
    for (microapp_id, caps) in grants.iter_mut() {
        for killed_cap in &killed {
            if caps.remove(*killed_cap) {
                let env_var = ADMIN_DOMAIN_KILL_SWITCHES
                    .iter()
                    .find(|(c, _)| c == killed_cap)
                    .map(|(_, e)| *e)
                    .unwrap_or("");
                stripped.push((microapp_id.clone(), (*killed_cap).to_string(), env_var));
            }
        }
    }
    stripped
}

/// One bootstrapped microapp: the router that the spawn loop
/// passes via `StdioSpawnOptions::admin_router`, plus the
/// deferred writer that boot binds post-spawn.
struct PerMicroappWire {
    router: Arc<DispatcherAdminRouter>,
    writer: Arc<DeferredAdminOutboundWriter>,
    /// `nexo/notify/pairing_status_changed`
    /// notifier sharing the same stdin queue as `writer`. Bound
    /// in [`AdminRpcBootstrap::bind_writer`] alongside the
    /// response writer so microapps see status transitions in
    /// real time instead of polling.
    pairing_notifier: Arc<crate::admin_adapters::DeferredPairingNotifier>,
    /// `nexo/notify/token_rotated` notifier
    /// sharing the same stdin queue. Bound alongside the
    /// pairing notifier in [`AdminRpcBootstrap::bind_writer`].
    /// The daemon-global `FsAuthRotator` pushes via the fanout;
    /// the fanout dispatches to this per-microapp deferred.
    token_rotated_notifier: Arc<crate::admin_adapters::DeferredTokenRotatedNotifier>,
}

/// Owns every shared admin RPC singleton + per-microapp wires.
/// Drop the bootstrap to cleanly tear down the prune task.
pub struct AdminRpcBootstrap {
    wires: BTreeMap<String, PerMicroappWire>,
    /// Pairing prune task — aborted on drop.
    prune_handle: Option<JoinHandle<()>>,
    /// Per-microapp firehose subscriber tasks.
    /// Each task reads from the shared broadcast channel and
    /// forwards filtered frames to the deferred writer for that
    /// microapp. Aborted on bootstrap drop.
    subscribe_handles: Vec<JoinHandle<()>>,
    /// Live emitter — `Arc` so test code can assert subscriber
    /// counts + spec callers can clone into the
    /// `TranscriptWriter::with_emitter` builder.
    event_emitter: Arc<dyn AgentEventEmitter>,
    /// Phase 81.20.x Stage 7 Phase 2 — shared pairing store
    /// clone. Daemon's post-`wire_plugin_registry` pairing-inbound
    /// subscribers consume this so plugin-published QR + state
    /// frames land in the same in-memory store the dispatcher
    /// reads when it handles `pairing/status`.
    pairing_store: Arc<dyn PairingChallengeStore>,
}

impl std::fmt::Debug for AdminRpcBootstrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminRpcBootstrap")
            .field("wired_microapps", &self.wires.keys().collect::<Vec<_>>())
            .field("prune_active", &self.prune_handle.is_some())
            .finish()
    }
}

impl Drop for AdminRpcBootstrap {
    fn drop(&mut self) {
        if let Some(h) = self.prune_handle.take() {
            h.abort();
        }
        for h in self.subscribe_handles.drain(..) {
            h.abort();
        }
    }
}

/// Errors surfaced from boot. A required-capability mismatch is
/// fail-fast — the operator misconfigured the grant matrix and
/// the microapp would otherwise crash on first call.
#[derive(Debug, thiserror::Error)]
pub enum AdminBootstrapError {
    /// Capability validation failed (required cap missing,
    /// orphan, etc.).
    #[error("admin capability boot validation failed: {0}")]
    CapabilityValidation(String),
    /// Extension declared a non-loopback HTTP
    /// bind without flipping
    /// `extensions.yaml.<id>.allow_external_bind = true`.
    /// Defense-in-depth — fail the boot rather than silently
    /// expose the microapp to LAN.
    #[error("extension `{microapp_id}` declares http_server bind=`{bind}` but `allow_external_bind` is false")]
    ExternalBindNotAllowed {
        /// Offending microapp id.
        microapp_id: String,
        /// Bind address from `plugin.toml`.
        bind: String,
    },
}

/// Inputs the daemon hands to [`AdminRpcBootstrap::build`].
/// Owned types so the bootstrap can stash references without
/// borrowing from the caller's frame.
pub struct AdminBootstrapInputs<'a> {
    /// Operator-managed config root (typically `./config`).
    pub config_dir: &'a Path,
    /// Resolved `secrets/` root (typically `./secrets`).
    pub secrets_root: &'a Path,
    /// SQLite audit DB path. `None` → `InMemoryAuditWriter`
    /// (volatile, suitable for ephemeral / dev daemons).
    pub audit_db: Option<&'a Path>,
    /// `extensions:` block from `extensions.yaml`.
    pub extensions_cfg: &'a ExtensionsConfig,
    /// Per-extension `[capabilities.admin]` block from each
    /// discovered `plugin.toml`. Keyed by extension id.
    pub admin_capabilities: &'a BTreeMap<String, AdminCapabilities>,
    /// Per-extension `[capabilities.http_server]`
    /// block from `plugin.toml`. Boot validates the bind policy
    /// against `extensions.yaml.<id>.allow_external_bind`. v0
    /// only checks the policy; the actual probe + monitor loop
    /// are spawned by main.rs after `initialize` returns. Keys
    /// match `admin_capabilities`; missing entries skip the
    /// bind check.
    pub http_server_capabilities: &'a BTreeMap<String, HttpServerCapability>,
    /// Reload signal — invoked by domain handlers
    /// after a successful yaml mutation.
    pub reload_signal: ReloadSignal,
    /// Optional transcripts reader. When `Some`,
    /// the agent_events admin domain (`list/read/search`) is
    /// installed on every dispatcher. `None` skips the domain
    /// (microapps see `-32601` for those methods).
    pub transcript_reader: Option<Arc<dyn TranscriptReader>>,
    /// Broker handle the
    /// `BrokerOutboundDispatcher` publishes to. `None` keeps the
    /// outbound surface disabled —
    /// `nexo/admin/processing/intervention` returns
    /// `-32603 channel_outbound dispatcher not configured`.
    /// Production wiring in `main.rs` always passes
    /// `Some(broker.clone())`.
    pub broker: Option<nexo_broker::AnyBroker>,
    /// Optional `TranscriptWriter` shared with
    /// the agent runtime. When `Some`, the dispatcher gains a
    /// `TranscriptWriterAppender` so
    /// `nexo/admin/processing/intervention` stamps operator
    /// replies on the agent transcript (the agent reads them on
    /// its next turn). When `None`, the channel send still
    /// happens but `ProcessingAck.transcript_stamped` reports
    /// `Some(false)`.
    pub transcript_writer: Option<Arc<nexo_core::agent::transcripts::TranscriptWriter>>,
    /// Operator pause-control store SHARED with
    /// the agent runtime. Boot constructs ONE
    /// `InMemoryProcessingControlStore`, hands it here for the
    /// admin RPC dispatcher (`pause`/`resume`/`intervention`
    /// handlers), and ALSO threads the same `Arc` to every
    /// `AgentRuntime` via `with_processing_store(...)`. The
    /// shared instance is what makes a `processing/pause` RPC
    /// reach the inbound loop on the very next message —
    /// without sharing, the dispatcher and runtime see two
    /// different stores and pause never reaches the runtime.
    /// When `None`, the dispatcher domain is disabled (admin
    /// RPC returns `processing domain not configured`) AND the
    /// runtime processes every inbound regardless of pause.
    pub processing_store: Option<Arc<dyn ProcessingControlStore>>,
    /// Multi-tenant SaaS registry.
    /// `None` keeps `nexo/admin/tenants/*` returning the typed
    /// `tenants domain not configured` error (single-tenant
    /// deployments). Production wires
    /// `crate::admin_adapters::TenantsYamlPatcher` against
    /// `config/tenants.yaml`.
    pub tenant_store: Option<Arc<dyn nexo_core::agent::admin_rpc::domains::tenants::TenantStore>>,
    /// MCP server registry CRUD. `None` keeps
    /// `nexo/admin/mcp/*` returning the typed `mcp domain not
    /// configured` error (operator manages mcp.yaml via CLI).
    /// Production wires
    /// `nexo_core::agent::admin_rpc::domains::mcp::McpYamlStore`
    /// against `<config_dir>/mcp.yaml`.
    pub mcp_store: Option<Arc<dyn nexo_core::agent::admin_rpc::domains::mcp::McpServerStore>>,
    /// Plugin doctor snapshot reader. `None`
    /// keeps `nexo/admin/plugins/doctor` returning the typed
    /// `plugins domain not configured` -32603. Production wires
    /// `crate::admin_adapters::LivePluginDoctorReader`.
    pub plugin_doctor:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::plugin_doctor::PluginDoctorReader>>,
    /// Shared plugin handles cell used by both the plugin restart
    /// adapter and the pairing-channels descriptor reader. `None`
    /// keeps `nexo/admin/pairing/channels` returning an empty list
    /// (plugins not yet loaded) — the dispatcher still wires the
    /// route so the admin doesn't see a `-32603` during boot.
    /// Production wires the same cell that backs
    /// [`crate::admin_adapters::LivePluginRestarter`].
    pub plugin_handles_cell: Option<crate::admin_adapters::SharedPluginHandles>,
    /// Install roots of every persona discovered at boot. Each
    /// entry points at a `<state>/personas/<id>-<ver>` directory
    /// that may ship its own `agents.d/*.yaml`. Empty default keeps
    /// existing tests / single-tenant setups working unchanged.
    /// Production wires the values from
    /// `nexo_persona_installer::discover_personas` results.
    pub persona_install_roots: Vec<std::path::PathBuf>,
    /// Manual plugin restart adapter.
    /// `None` keeps `nexo/admin/plugins/restart` returning the
    /// typed `plugin restart domain not configured` -32603.
    /// Production wires `crate::admin_adapters::LivePluginRestarter`
    /// around the daemon's plugin handles snapshot.
    pub plugin_restarter:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::plugin_restart::PluginRestarter>>,
    /// Phase 97.1 — runtime plugin install / scan / uninstall.
    /// `None` keeps `nexo/admin/plugins/{scan,install,uninstall}`
    /// returning the typed `plugin install domain not configured`
    /// -32603. Production wires a late-bind installer behind a
    /// shared cell so `LivePluginInstaller` can be constructed
    /// AFTER `wire_plugin_registry` returns + injected once the
    /// per-plugin Arc fixtures (factory registry, handles cell,
    /// router shares) are populated.
    pub plugin_installer:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::plugin_install::PluginInstaller>>,
    /// Phase 98.10/98.11 — plugin discovery reader.
    /// `None` keeps `nexo/admin/plugins/{search,compat_check,
    /// refresh_index}` returning the typed `plugin discovery domain
    /// not configured` -32603. Production wires
    /// `crate::discovery_adapter::DefaultDiscoveryAdapter`.
    pub plugin_discovery:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::plugin_discovery::DiscoveryReader>>,
    /// Long-term memory query reader. `None`
    /// keeps `nexo/admin/memory/query` returning the typed
    /// `memory domain not configured` -32603. Production wires
    /// `crate::admin_adapters::LiveMemoryReader` around the daemon's
    /// existing `LongTermMemory` instance.
    pub memory_reader: Option<Arc<dyn nexo_core::agent::admin_rpc::domains::memory::MemoryReader>>,
    /// Snapshot list reader. `None`
    /// keeps `nexo/admin/memory/list_snapshots` returning the
    /// typed `memory snapshot domain not configured` -32603.
    /// Production wires `crate::admin_adapters::LiveMemorySnapshotReader`.
    pub memory_snapshot_reader:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::memory::MemorySnapshotReader>>,
    /// Secrets store. `None` keeps
    /// `nexo/admin/secrets/write` returning the typed
    /// `secrets domain not configured` -32603. Production wires
    /// `crate::secrets_store::FsSecretsStore` rooted at
    /// `<state_root>/secrets/` (mode 0600 file write +
    /// `std::env::set_var` so existing LLM clients see the new
    /// value without a daemon restart).
    pub secrets_store: Option<Arc<dyn nexo_core::agent::admin_rpc::domains::secrets::SecretsStore>>,
    /// Daemon-side LLM provider probe. `None`
    /// keeps `nexo/admin/llm_providers/probe` returning the
    /// typed `llm_providers probe not configured` -32603.
    /// Production wires
    /// `crate::llm_provider_probe::HttpLlmProviderProbe` against
    /// the existing `LlmYamlPatcher` so the probe reflects
    /// the same config agent traffic would resolve.
    pub llm_provider_probe:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::llm_providers::LlmProvidersProbe>>,
    /// Runtime LLM completer. `None` keeps
    /// `nexo/admin/llm/complete` returning the typed
    /// `llm completer not configured` -32603. Production wires
    /// `crate::llm_completer::RegistryLlmCompleter::new(
    /// registry, llm_cfg)` so extensions / microapps can
    /// delegate completions through the daemon's existing
    /// provider plumbing.
    pub llm_completer: Option<Arc<dyn nexo_core::agent::admin_rpc::domains::llm::LlmCompleter>>,
    /// Snapshot of every LLM provider factory the daemon registered
    /// at boot. Drives the `nexo/admin/llm_providers/catalog` RPC.
    /// Production passes
    /// `LlmRegistry::catalog()` mapped into the wire shape so SPA
    /// wizards render strict provider/model dropdowns. Empty vec
    /// disables the RPC silently (caller error message guides ops).
    pub llm_provider_catalog: Vec<nexo_tool_meta::admin::llm_providers::LlmProviderCatalogEntry>,
    /// Operator bearer rotator. Test override:
    /// when `Some`, the bootstrap installs this rotator
    /// directly without consulting [`Self::auth_token_path`] /
    /// [`Self::auth_initial_hash`]. Useful for mocks.
    pub auth_rotator: Option<Arc<dyn nexo_core::agent::admin_rpc::domains::auth::AuthRotator>>,
    /// Canonical operator-token file path.
    /// Production passes
    /// `<state_root>/secrets/operator_token.txt`. When `Some`
    /// AND [`Self::auth_initial_hash`] is also `Some` AND
    /// [`Self::auth_rotator`] is `None`, the bootstrap builds
    /// `FsAuthRotator` internally with the per-microapp fanout
    /// notifier + the shared firehose `AgentEventEmitter` so a
    /// rotation lands BOTH the live `nexo/notify/token_rotated`
    /// AND the durable
    /// `AgentEventKind::SecurityEvent::TokenRotated` audit row.
    pub auth_token_path: Option<std::path::PathBuf>,
    /// Initial operator-token-hash (16-char
    /// sha256-hex prefix). Computed by the caller from the
    /// operator-supplied env var at boot. Used as the very first
    /// rotation's `old_hash` so microapp listeners can verify
    /// the message matches the token they hold.
    pub auth_initial_hash: Option<String>,
    /// Skills domain store. `None`
    /// keeps `nexo/admin/skills/*` returning the typed
    /// `skills domain not configured` -32603. Production wires
    /// `crate::admin_adapters::FsSkillsStore` against the same
    /// skills root the runtime `SkillLoader` reads from so admin
    /// writes land where the runtime reads.
    pub skills_store: Option<Arc<dyn nexo_core::agent::admin_rpc::domains::skills::SkillsStore>>,
    /// Escalations store. `None` keeps
    /// `nexo/admin/escalations/*` returning the typed
    /// `escalations domain not configured` -32603. Production
    /// wires the in-memory store + the future SQLite adapter.
    pub escalation_store:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::escalations::EscalationStore>>,
    /// Durable agent-event log. When
    /// `Some`, boot composes the live broadcast emitter with the
    /// log into a `Tee([Broadcast, Log])` so every emit (transcripts
    /// + processing state changes + escalation requested/resolved)
    /// also lands in SQLite for backfill across daemon restarts.
    /// `None` keeps the broadcast-only behaviour — backfill RPC
    /// then returns transcripts JSONL only via `TranscriptReaderFs`,
    /// without the durable non-transcript kinds.
    ///
    /// Concrete-typed (`Arc<SqliteAgentEventLog>`) on purpose so
    /// boot can use the same handle for both the emitter side
    /// (Tee composition via the `AgentEventEmitter` impl) AND the
    /// read side (constructing `MergingAgentEventReader` via the
    /// `AgentEventLog` impl). MSRV 1.80 doesn't support trait
    /// object upcasting yet, so a single-typed handle keeps both
    /// uses free of awkward casting helpers. Tests use the same
    /// type via `SqliteAgentEventLog::open_memory()`.
    ///
    /// Boot supervisor calls `sweep_retention(retention_days,
    /// max_rows)` on the same scheduler as the audit-log sweep
    /// when the log is wired.
    pub agent_event_log: Option<Arc<nexo_core::agent::admin_rpc::SqliteAgentEventLog>>,
    /// Channel credential persisters that bridge
    /// `nexo/admin/credentials/register` to per-channel runtime
    /// state (yaml accounts list + secret file). Boot iterates
    /// this list and calls `dispatcher.register_persister` for
    /// each. Empty by default keeps the opaque-only path (no
    /// regression for non-channel-aware callers). Production
    /// pushes
    /// [`crate::persisters::TelegramPersister`],
    /// [`crate::persisters::EmailPersister`],
    /// [`crate::persisters::WhatsappPersister`] when the matching
    /// plugin is enabled in `extensions.yaml`.
    pub persisters:
        Vec<Arc<dyn nexo_core::agent::admin_rpc::domains::credentials::ChannelCredentialPersister>>,
    /// Per-channel pairing trigger registry.
    /// Empty default keeps the legacy "Pending forever" behavior
    /// (admin pairing/start creates a row but never pushes QR);
    /// production passes one entry per channel that has a
    /// registered plugin (today: `whatsapp` → `WhatsappPairingTrigger`,
    /// future: `telegram` → telegram-link, etc).
    pub pairing_triggers: nexo_core::agent::admin_rpc::pairing_trigger::PairingChannelTriggers,
    /// Phase 81.33.b.real Stage 4 — manifest-driven plugin admin
    /// router. Daemon builds this from
    /// `wire.plugin_handles[..].manifest().plugin.admin` at boot
    /// (longest-prefix-first; reserved-prefix collisions warn-
    /// logged + skipped). `None` keeps the dispatcher on the
    /// legacy `.with_<plugin>_handle()` typed path; production
    /// always passes `Some`.
    pub plugin_admin_router: Option<std::sync::Arc<nexo_pairing::plugin_admin::PluginAdminRouter>>,
}

impl AdminRpcBootstrap {
    /// Build every per-microapp wire. Returns `Ok(None)` when no
    /// extension declares admin capabilities — the daemon then
    /// runs without any admin RPC plumbing (zero overhead).
    pub async fn build(
        inputs: AdminBootstrapInputs<'_>,
    ) -> Result<Option<Self>, AdminBootstrapError> {
        let firehose_on = agent_events_enabled();
        Self::build_inner(inputs, firehose_on).await
    }

    /// Test-only entry point that overrides the
    /// `NEXO_MICROAPP_AGENT_EVENTS_ENABLED` env-var read with an
    /// explicit bool. Production paths use [`Self::build`].
    #[doc(hidden)]
    pub async fn build_with_firehose(
        inputs: AdminBootstrapInputs<'_>,
        firehose_on: bool,
    ) -> Result<Option<Self>, AdminBootstrapError> {
        Self::build_inner(inputs, firehose_on).await
    }

    async fn build_inner(
        inputs: AdminBootstrapInputs<'_>,
        firehose_on: bool,
    ) -> Result<Option<Self>, AdminBootstrapError> {
        // Filter to extensions that actually declare admin caps;
        // skip the rest entirely so a daemon with no admin-using
        // microapps pays no cost.
        let declared: Vec<(String, AdminCapabilityDecl)> = inputs
            .admin_capabilities
            .iter()
            .filter(|(_, d)| !d.required.is_empty() || !d.optional.is_empty())
            .map(|(id, d): (&String, &AdminCapabilities)| {
                (
                    id.clone(),
                    AdminCapabilityDecl {
                        required: d.required.clone(),
                        optional: d.optional.clone(),
                    },
                )
            })
            .collect();
        if declared.is_empty() {
            return Ok(None);
        }

        // Operator grants from `extensions.yaml.entries.<id>.capabilities_grant`.
        let mut grants: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for (id, entry) in &inputs.extensions_cfg.entries {
            grants.insert(id.clone(), entry.capabilities_grant.clone());
        }

        // 2-tier validation: required missing → error, optional
        // missing → warn, orphan grant → warn.
        let report = validate_capabilities_at_boot(&declared, &grants);
        if !report.errors.is_empty() {
            let detail = report
                .errors
                .iter()
                .map(|e| format!("{e:?}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(AdminBootstrapError::CapabilityValidation(detail));
        }
        for warn in &report.warns {
            tracing::warn!(detail = ?warn, "admin capability boot warning");
        }
        // Apply per-domain kill switches
        // declared in `crates/setup/src/capabilities.rs::INVENTORY`.
        // When the operator exports
        // `NEXO_MICROAPP_ADMIN_<DOMAIN>_ENABLED=0` the matching
        // capability is stripped from every microapp's grant set
        // BEFORE the dispatcher CapabilitySet is built, so the
        // verb returns `CapabilityNotGranted -32004` regardless
        // of the operator-edited `extensions.yaml.<id>.capabilities_grant`.
        // Previously the INVENTORY entries were reported by
        // `agent doctor capabilities` but had zero functional
        // effect (silent operator-misleading bug).
        let mut effective_grants = report.grants.clone();
        let stripped =
            apply_admin_domain_kill_switches(&mut effective_grants, |k| std::env::var(k).ok());
        for (microapp_id, capability, env_var) in &stripped {
            tracing::warn!(
                target: "admin.boot.kill_switch",
                microapp_id = %microapp_id,
                capability = %capability,
                env_var = %env_var,
                "admin capability stripped by domain kill switch"
            );
        }
        let capability_set = CapabilitySet::from_grants(effective_grants);

        // Validate http_server bind policy. Each
        // extension that declares a non-loopback bind must have
        // `extensions.yaml.<id>.allow_external_bind = true` or
        // boot fails. Loopback (127.0.0.1 / ::1 / localhost) is
        // always allowed.
        for (microapp_id, decl) in inputs.http_server_capabilities {
            let allow_external = inputs
                .extensions_cfg
                .entries
                .get(microapp_id)
                .map(|e| e.allow_external_bind)
                .unwrap_or(false);
            if let Err(bind) = crate::http_supervisor::validate_bind_policy(decl, allow_external) {
                return Err(AdminBootstrapError::ExternalBindNotAllowed {
                    microapp_id: microapp_id.clone(),
                    bind,
                });
            }
        }

        // Audit writer — single instance shared across every
        // dispatcher. Keep the concrete
        // `SqliteAdminAuditWriter` Arc when available so the
        // same instance can satisfy both `AdminAuditWriter` (for
        // the dispatcher's audit append) AND `AdminAuditReader`
        // (for the new `nexo/admin/microapp_audit/tail` RPC).
        // The `InMemoryAuditWriter` fallback (no audit DB
        // configured) doesn't implement reader, so the
        // microapp_audit domain stays unconfigured in that case.
        let audit_sqlite: Option<Arc<SqliteAdminAuditWriter>> = match inputs.audit_db {
            Some(path) => match SqliteAdminAuditWriter::open(path).await {
                Ok(w) => {
                    tracing::info!(path=%path.display(), "admin audit DB opened");
                    Some(Arc::new(w))
                }
                Err(e) => {
                    tracing::warn!(
                        path=%path.display(),
                        error=%e,
                        "admin audit DB open failed; falling back to InMemoryAuditWriter",
                    );
                    None
                }
            },
            None => None,
        };
        let audit: Arc<dyn AdminAuditWriter> = match &audit_sqlite {
            Some(arc) => arc.clone(),
            None => Arc::new(InMemoryAuditWriter::new()),
        };
        let audit_reader: Option<Arc<dyn nexo_core::agent::admin_rpc::AdminAuditReader>> =
            audit_sqlite.as_ref().map(|arc| arc.clone() as _);

        // Filesystem-side adapters — singletons.
        // Persona install_roots intentionally NOT threaded here.
        // Personas are TEMPLATES for the create-agent wizard, not
        // live agents; surfacing their `agents.d/` files via
        // `agents/list` and `agents/get` conflated installs with
        // instances and produced ghost "agent" entries the
        // operator never created. Templates are exposed instead
        // via `nexo/admin/personas/*` (see persona admin domain).
        let agents_yaml = Arc::new(AgentsYamlPatcher::new(
            inputs.config_dir.join("agents.yaml"),
        ));
        let llm_yaml = Arc::new(LlmYamlPatcherFs::new(inputs.config_dir.join("llm.yaml")));
        let credential_store = Arc::new(FilesystemCredentialStore::new(inputs.secrets_root));
        let pairing_store = Arc::new(InMemoryPairingChallengeStore::new(Duration::from_secs(
            5 * 60,
        )));

        // Spawn the pairing prune task — every 30 s.
        let prune_handle = {
            let store = pairing_store.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    let removed = store.prune_expired();
                    if removed > 0 {
                        tracing::debug!(removed, "pruned expired pairing challenges");
                    }
                }
            })
        };

        // Broadcast emitter shared across every
        // microapp wire. INVENTORY env (`NEXO_MICROAPP_AGENT_EVENTS_ENABLED`)
        // forces a noop emitter so the boot path costs zero
        // when the operator hard-disables the firehose. The
        // backfill admin domain still installs (so a microapp
        // with `transcripts_read` keeps querying past sessions)
        // — only the live notification stream is silenced.
        // `broadcast` is the concrete handle the boot subscribe
        // path needs (calls `subscribe()` which is not on the
        // trait); `event_emitter` is the trait handle handed to
        // `TranscriptWriter::with_emitter`. Both wrap the same
        // underlying broadcast when the firehose is on.
        let broadcast: Option<Arc<BroadcastAgentEventEmitter>> = if firehose_on {
            Some(Arc::new(BroadcastAgentEventEmitter::new()))
        } else {
            tracing::warn!(
                "{} disabled — agent_event firehose silenced; backfill RPC still works",
                ENV_AGENT_EVENTS_ENABLED,
            );
            None
        };
        // When boot wires a durable
        // `SqliteAgentEventLog`, compose `Tee([Broadcast, Log])`
        // so every emit reaches both live subscribers AND the
        // durable backfill source. Without a log, the broadcast
        // (or noop) emitter passes through unchanged.
        let event_emitter: Arc<dyn AgentEventEmitter> =
            match (broadcast.clone(), inputs.agent_event_log.clone()) {
                (Some(b), Some(log)) => Arc::new(
                    nexo_core::agent::agent_events::TeeAgentEventEmitter::with_sinks(vec![
                        b as Arc<dyn AgentEventEmitter>,
                        log as Arc<dyn AgentEventEmitter>,
                    ]),
                ),
                (Some(b), None) => b,
                (None, Some(log)) => log as Arc<dyn AgentEventEmitter>,
                (None, None) => Arc::new(NoopAgentEventEmitter),
            };

        // Build one (router, deferred writer) per declared microapp.
        // Pairing notifier is intentionally omitted for v1: the
        // notifier needs an extra wire from boot's `outbox_tx` to
        // the same sender the deferred writer drains, which is
        // chicken-and-egg (sender created inside `spawn_with`).
        // Microapps fall back to polling `pairing/status` until a
        // future follow-up exposes a separate notification queue
        // independent of the response writer.
        let mut wires = BTreeMap::new();
        let mut subscribe_handles: Vec<JoinHandle<()>> = Vec::new();
        // Fan-out notifier for
        // `nexo/notify/token_rotated`. Singleton across the
        // daemon; the global `FsAuthRotator` writes here, and we
        // register one `DeferredTokenRotatedNotifier` per
        // microapp inside the loop so a rotation fans out to
        // every connected listener.
        let auth_token_rotated_fanout =
            Arc::new(crate::admin_adapters::FanoutTokenRotatedNotifier::new());
        for (id, _decl) in &declared {
            let writer = Arc::new(DeferredAdminOutboundWriter::new());
            // Separate notification
            // queue for `nexo/notify/pairing_status_changed` so
            // microapps stop polling pairing/status on a 1-2 s
            // cadence. Bound post-spawn alongside the response
            // writer in `bind_writer`.
            let pairing_notifier = Arc::new(crate::admin_adapters::DeferredPairingNotifier::new());
            // Per-microapp deferred token-rotated
            // notifier, registered with the daemon-global fanout
            // below so a single rotate fans out to every connected
            // microapp.
            let token_rotated_notifier =
                Arc::new(crate::admin_adapters::DeferredTokenRotatedNotifier::new());
            auth_token_rotated_fanout.add(token_rotated_notifier.clone());

            let mut dispatcher = AdminRpcDispatcher::new()
                .with_capabilities(capability_set.clone())
                .with_audit_writer(audit.clone());
            // Install the audit-tail
            // read surface only when the SQLite writer is
            // available. InMemoryAuditWriter fallback omits
            // the reader → `nexo/admin/microapp_audit/tail`
            // returns "domain not configured" until operator
            // sets `audit_db` in config.
            if let Some(reader) = audit_reader.clone() {
                dispatcher = dispatcher.with_audit_reader(reader);
            }
            dispatcher = dispatcher
                .with_agents_domain(agents_yaml.clone(), inputs.reload_signal.clone())
                .with_credentials_domain(credential_store.clone())
                .with_pairing_domain(pairing_store.clone(), Some(pairing_notifier.clone()))
                .with_pairing_triggers(inputs.pairing_triggers.clone());
            // Phase 81.33.b.real Stage 4 — manifest-driven plugin
            // admin router. Wired here as a shared `Arc<>` so the
            // daemon can populate it AFTER `wire_plugin_registry`
            // returns (admin_bootstrap runs BEFORE wire today; the
            // router is empty at this point and the daemon registers
            // entries post-wire). The dispatcher consults the router
            // on every method that's unknown to the static map.
            if let (Some(router), Some(broker_handle)) =
                (inputs.plugin_admin_router.clone(), inputs.broker.clone())
            {
                dispatcher = dispatcher.with_plugin_admin_router(router, broker_handle);
            }
            // Phase 81.20.x Stage 7 BC.2 — `with_wa_bot_handle` block
            // removed. Whatsapp's `nexo/admin/whatsapp/*` admin RPCs
            // are now routed via `PluginAdminRouter` (Stage 4 wired
            // above through `with_plugin_admin_router`); the legacy
            // typed handle is redundant. With this drop the
            // admin_bootstrap module no longer imports the
            // `nexo_plugin_whatsapp::bot_registry` path.
            dispatcher = dispatcher
                .with_llm_providers_domain(llm_yaml.clone())
                .with_llm_provider_catalog(inputs.llm_provider_catalog.clone())
                .with_channels_domain();
            // Schema-driven upsert lookup. Reuses
            // the SAME catalog snapshot the SPA renders against,
            // so the schema validated server-side is identical
            // to the schema the wizard rendered. No drift
            // possible.
            if !inputs.llm_provider_catalog.is_empty() {
                let catalog_arc = Arc::new(inputs.llm_provider_catalog.clone());
                let lookup = crate::admin_adapters::CatalogFactorySchema::new(catalog_arc);
                dispatcher = dispatcher.with_llm_factory_schema(lookup);
            }
            // OAuth verifier store + sweep task.
            // Cap 100 concurrent sessions; each entry ~256B so the
            // worst case is ~25 KB. Sweep every 60s drops expired
            // entries; abandoned wizards never accumulate past TTL.
            let oauth_store = nexo_llm_auth::InMemoryVerifierStore::new(100);
            dispatcher = dispatcher.with_oauth_verifier_store(oauth_store.clone());
            // Spawn the TTL sweep. Detached: the daemon outlives the
            // bootstrap, so dropping the JoinHandle is fine. Inside
            // the loop we use `tokio::time::interval` so a slow tick
            // does not pile up.
            let store_for_sweep = oauth_store.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    use nexo_llm_auth::VerifierStore;
                    store_for_sweep.sweep_expired().await;
                }
            });
            if let Some(reader) = inputs.transcript_reader.clone() {
                dispatcher = dispatcher.with_agent_events_domain(reader);
            }
            // Wire the production
            // BrokerOutboundDispatcher when the broker handle is
            // available. Without it, `processing.intervention`
            // returns the typed "channel_outbound dispatcher not
            // configured" error so callers diagnose the wire-up
            // gap clearly.
            if let Some(broker) = inputs.broker.clone() {
                #[allow(unused_mut)]
                let mut outbound = crate::admin_adapters::BrokerOutboundDispatcher::new(broker);
                // Phase 81.20.x F3-followup — register the translators
                // every shipped canonical plugin provides. The outbound
                // dispatcher now covers email, telegram, and (cfg-gated)
                // whatsapp, unblocking admin RPC `processing.intervention.reply`
                // + the MCP autonomous worker's `plugin_channel_send` tool
                // against any of them.
                outbound = outbound
                    .with_translator(Box::new(crate::admin_adapters::EmailTranslator))
                    .with_translator(Box::new(crate::admin_adapters::TelegramTranslator))
                    .with_translator(Box::new(crate::admin_adapters::WhatsAppTranslator));
                dispatcher = dispatcher.with_channel_outbound(Arc::new(outbound));
            }
            // Wire the production transcript
            // appender when boot has the writer handle. Without
            // it, intervention(Reply) still dispatches via the
            // outbound adapter but the ack reports
            // `transcript_stamped: false`.
            if let Some(writer) = inputs.transcript_writer.clone() {
                let appender =
                    Arc::new(crate::admin_adapters::TranscriptWriterAppender::new(writer));
                dispatcher = dispatcher.with_transcript_appender(appender);
            }
            // Install the processing-control
            // domain when boot has a shared store. Same `Arc`
            // is also handed to every runtime via
            // `Runtime::with_processing_store` so a pause RPC
            // reaches the inbound loop on the next message.
            if let Some(store) = inputs.processing_store.clone() {
                dispatcher = dispatcher.with_processing_domain(store);
            }
            // Install the tenants
            // domain when boot has the production
            // `TenantsYamlPatcher` adapter. Without it,
            // `nexo/admin/tenants/*` returns the typed
            // `tenants domain not configured` -32603 so the
            // microapp surfaces a clear wire-up gap.
            if let Some(store) = inputs.tenant_store.clone() {
                dispatcher = dispatcher.with_tenants_domain(store);
            }
            // Install the MCP servers domain
            // when boot has the production yaml adapter wired.
            // Without it, `nexo/admin/mcp/*` returns the typed
            // `mcp domain not configured` -32603 so the plugin
            // admin surfaces a clear wire-up gap (the placeholder
            // page never goes live).
            if let Some(store) = inputs.mcp_store.clone() {
                dispatcher = dispatcher.with_mcp_domain(store);
            }
            // Install the plugin doctor reader
            // when boot has the production live impl wired. Without
            // it, `nexo/admin/plugins/doctor` returns the typed
            // `plugins domain not configured` -32603.
            if let Some(reader) = inputs.plugin_doctor.clone() {
                dispatcher = dispatcher.with_plugin_doctor(reader);
            }
            // Phase 81.31 — persona multi-locale store + snapshot
            // reader. Same singleton powers both `agents/get`
            // enrichment (`persona_locales`) and
            // `persona/save_localized` write. Reuses
            // `persona_install_roots` (same vec wired into
            // `AgentsYamlPatcher`).
            let persona_store = crate::admin_adapters::FilesystemPersonaStore::new(
                inputs.config_dir.to_path_buf(),
                inputs.persona_install_roots.clone(),
            );
            dispatcher = dispatcher
                .with_persona_snapshot_reader(persona_store.clone())
                .with_persona_store(persona_store);

            // Persona TEMPLATE catalog (read-only). Distinct from
            // the locale store above: this surface exposes the
            // installed persona packs as "use as template" rows
            // for the create-agent wizard. Powered by the same
            // `persona_install_roots` snapshot the daemon
            // discovered at boot. Personas listed here are NEVER
            // auto-merged into `cfg.agents`.
            let persona_catalog = crate::admin_adapters::FilesystemPersonaCatalog::new(
                inputs.persona_install_roots.clone(),
            );
            dispatcher = dispatcher.with_persona_catalog(persona_catalog);

            // Install the pairing-channels descriptor reader,
            // built here so we can reuse the `credential_store`
            // already wired into the credentials domain. Cell is
            // typically empty at this point in boot; main.rs
            // populates it after `wire_plugin_registry`. Until
            // then `pairing/channels` returns an empty list — the
            // admin renders its empty-state UI cleanly.
            if let Some(cell) = inputs.plugin_handles_cell.clone() {
                let reader = crate::admin_adapters::PairingChannelsReaderImpl::new(
                    cell,
                    credential_store.clone(),
                );
                dispatcher = dispatcher.with_pairing_channels(reader);
            }
            // Install the manual
            // plugin restart adapter. Without it,
            // `nexo/admin/plugins/restart` returns the typed
            // `plugin restart domain not configured` -32603.
            if let Some(restarter) = inputs.plugin_restarter.clone() {
                dispatcher = dispatcher.with_plugin_restarter(restarter);
            }
            // Phase 97.1 — install/scan/uninstall adapter.
            // Mirror shape: `None` keeps the three RPC verbs
            // returning the typed -32603; production wires the
            // late-bind `LivePluginInstaller`.
            if let Some(installer) = inputs.plugin_installer.clone() {
                dispatcher = dispatcher.with_plugin_installer(installer);
            }
            // Phase 98.10/98.11 — discovery reader. `None` keeps
            // the 3 RPC verbs (`search`/`compat_check`/`refresh_index`)
            // returning the typed -32603; production wires
            // `DefaultDiscoveryAdapter`.
            if let Some(reader) = inputs.plugin_discovery.clone() {
                dispatcher = dispatcher.with_plugin_discovery(reader);
            }
            // Install the memory query reader.
            // Without it, `nexo/admin/memory/query` returns the
            // typed `memory domain not configured` -32603.
            if let Some(reader) = inputs.memory_reader.clone() {
                dispatcher = dispatcher.with_memory_reader(reader);
            }
            // Install the snapshot
            // list reader.
            if let Some(reader) = inputs.memory_snapshot_reader.clone() {
                dispatcher = dispatcher.with_memory_snapshot_reader(reader);
            }
            // Install the secrets domain when
            // boot has the production `FsSecretsStore` adapter.
            // Without it, `nexo/admin/secrets/write` returns the
            // typed `secrets domain not configured` -32603 so
            // the microapp wizard surfaces a clear wire-up gap.
            if let Some(store) = inputs.secrets_store.clone() {
                dispatcher = dispatcher.with_secrets_domain(store);
            }
            // Install the daemon-side LLM
            // provider probe. Caller-supplied (e.g. test mocks)
            // wins; otherwise default-construct the production
            // `HttpLlmProviderProbe` against the local
            // `llm_yaml` so admin/llm_providers/probe just
            // works out of the box.
            let probe: Arc<
                dyn nexo_core::agent::admin_rpc::domains::llm_providers::LlmProvidersProbe,
            > = inputs.llm_provider_probe.clone().unwrap_or_else(|| {
                crate::llm_provider_probe::HttpLlmProviderProbe::new(llm_yaml.clone())
            });
            dispatcher = dispatcher.with_llm_provider_probe(probe);
            // Install the runtime LLM
            // completer (drives `nexo/admin/llm/complete`).
            // Caller-supplied wins; with no override the
            // bootstrap leaves `None` so the dispatch path
            // returns `Internal("llm completer not configured")`.
            // Production wires this from `main.rs` against the
            // daemon's `LlmRegistry` + the live `LlmConfig`
            // ArcSwap handle so completions reflect the same
            // provider config the agent loop sees.
            if let Some(c) = inputs.llm_completer.clone() {
                dispatcher = dispatcher.with_llm_completer(c);
            }
            // Install the auth rotator. Test
            // override (`auth_rotator: Some(...)`) wins;
            // otherwise build the production `FsAuthRotator` if
            // the caller supplied `auth_token_path` AND
            // `auth_initial_hash`. The shared
            // `auth_token_rotated_fanout` registered above gets
            // a `DeferredTokenRotatedNotifier` per microapp so
            // a single rotation reaches every connected
            // listener. When neither path applies, the dispatcher
            // returns the typed `auth rotator not configured`
            // -32603 so callers diagnose the gap clearly.
            if let Some(rotator) = inputs.auth_rotator.clone() {
                dispatcher = dispatcher.with_auth_rotator(rotator);
            } else if let (Some(token_path), Some(initial_hash)) = (
                inputs.auth_token_path.clone(),
                inputs.auth_initial_hash.clone(),
            ) {
                let rotator = crate::auth_rotator::FsAuthRotator::new(
                    token_path,
                    auth_token_rotated_fanout.clone()
                        as Arc<
                            dyn nexo_core::agent::admin_rpc::domains::auth::TokenRotatedNotifier,
                        >,
                    event_emitter.clone(),
                    initial_hash,
                );
                dispatcher = dispatcher.with_auth_rotator(rotator);
            }
            // Register per-channel credential
            // persisters. Each push (telegram/email/whatsapp +
            // future) becomes a dispatcher registry entry that
            // `credentials/register` looks up by `channel`. The
            // dispatcher panics on duplicate channel; main.rs is
            // responsible for ensuring each channel registers at
            // most one persister (typically tied to whether the
            // matching plugin is enabled in extensions.yaml).
            for p in &inputs.persisters {
                dispatcher.register_persister(p.clone());
            }
            // Install the skills domain
            // when boot has the production `FsSkillsStore`.
            if let Some(store) = inputs.skills_store.clone() {
                dispatcher = dispatcher.with_skills_domain(store);
            }
            // Install the escalations
            // domain when boot has a store adapter wired.
            if let Some(store) = inputs.escalation_store.clone() {
                dispatcher = dispatcher.with_escalations_domain(store);
            }

            // Spawn a per-microapp subscriber when
            // the operator granted `transcripts_subscribe` or
            // `agent_events_subscribe_all`. v0 emits only
            // `TranscriptAppended` so both caps subscribe to the
            // same stream; the reserved `_subscribe_all` slot
            // just takes the union of future kinds without any
            // per-microapp re-config.
            if let Some(b) = broadcast.as_ref() {
                let granted = capability_set.granted_for(id);
                let wants_transcripts = granted
                    .map(|g| {
                        g.contains(CAP_TRANSCRIPTS_SUBSCRIBE)
                            || g.contains(CAP_AGENT_EVENTS_SUBSCRIBE_ALL)
                    })
                    .unwrap_or(false);
                if wants_transcripts {
                    let mut rx = b.subscribe();
                    let writer_clone: Arc<DeferredAdminOutboundWriter> = writer.clone();
                    let microapp_id = id.clone();
                    let handle = tokio::spawn(async move {
                        firehose_subscriber_loop(microapp_id, &mut rx, writer_clone).await;
                    });
                    subscribe_handles.push(handle);
                }
            }

            let router = Arc::new(DispatcherAdminRouter::new(
                Arc::new(dispatcher),
                writer.clone(),
            ));
            wires.insert(
                id.clone(),
                PerMicroappWire {
                    router,
                    writer,
                    pairing_notifier,
                    token_rotated_notifier,
                },
            );
        }

        Ok(Some(Self {
            wires,
            prune_handle: Some(prune_handle),
            subscribe_handles,
            event_emitter,
            pairing_store: pairing_store.clone(),
        }))
    }

    /// Shared in-memory pairing challenge store.
    ///
    /// Phase 81.20.x Stage 7 Phase 2 — daemon's
    /// post-`wire_plugin_registry` pairing-inbound subscribers use
    /// this Arc to push plugin-published QR + state frames into
    /// the same store the dispatcher reads on `pairing/status`.
    pub fn pairing_store(&self) -> Arc<dyn PairingChallengeStore> {
        self.pairing_store.clone()
    }

    /// Clone of the shared firehose emitter.
    /// Boot wiring threads this into every `TranscriptWriter`
    /// via `with_emitter` so appended entries reach the
    /// broadcast bus. When the bootstrap was built with
    /// `NEXO_MICROAPP_AGENT_EVENTS_ENABLED=0` this returns the
    /// `NoopAgentEventEmitter` — writers stay correct + cheap.
    pub fn event_emitter(&self) -> Arc<dyn AgentEventEmitter> {
        self.event_emitter.clone()
    }

    /// Build the spawn options the extension host should use for
    /// `extension_id`. When the id has no admin wiring (microapp
    /// declared no admin caps), returns `None` — caller falls
    /// back to the existing default options.
    pub fn spawn_options_for(
        &self,
        extension_id: &str,
        base: StdioSpawnOptions,
    ) -> Option<StdioSpawnOptions> {
        let wire = self.wires.get(extension_id)?;
        let router: SharedAdminRouter = wire.router.clone();
        Some(StdioSpawnOptions {
            admin_router: Some(router),
            ..base
        })
    }

    /// Bind the live extension stdin queue post-spawn. After
    /// this call, every admin response routed through the
    /// dispatcher flows out through the extension's stdin AND
    /// every `nexo/notify/pairing_status_changed` frame from
    /// the deferred pairing notifier reaches the same queue
    /// (Closes the chicken-and-egg
    /// that previously left microapps polling
    /// `pairing/status`).
    pub fn bind_writer(&self, extension_id: &str, sender: tokio::sync::mpsc::Sender<String>) {
        if let Some(wire) = self.wires.get(extension_id) {
            wire.writer.bind(sender.clone());
            wire.pairing_notifier.bind(sender.clone());
            // Same stdin queue carries the
            // token-rotated notify so microapp's
            // `LiveTokenState` listener swaps in-place.
            wire.token_rotated_notifier.bind(sender);
        }
    }

    /// Number of subscribe tasks the boot wired.
    /// Equals the count of microapps whose grants include
    /// `transcripts_subscribe` or `agent_events_subscribe_all`.
    /// `0` when the firehose is INVENTORY-disabled.
    pub fn subscribe_task_count(&self) -> usize {
        self.subscribe_handles.len()
    }

    /// `true` when at least one microapp has an admin wire.
    pub fn is_active(&self) -> bool {
        !self.wires.is_empty()
    }

    /// Microapp ids carrying an admin wire — for boot diagnostics.
    pub fn wired_ids(&self) -> Vec<String> {
        self.wires.keys().cloned().collect()
    }
}

/// Per-microapp subscriber loop. Reads from the broadcast
/// receiver, serializes each frame as a JSON-RPC notification
/// (no `id`), and writes it through the deferred outbound
/// writer. `Lagged(n)` events surface as a single `warn` log
/// — microapps that miss frames re-issue
/// `agent_events/read` with their last-seen `seq`.
async fn firehose_subscriber_loop(
    microapp_id: String,
    rx: &mut tokio::sync::broadcast::Receiver<AgentEventKind>,
    writer: Arc<DeferredAdminOutboundWriter>,
) {
    use nexo_core::agent::admin_rpc::AdminOutboundWriter;
    loop {
        match rx.recv().await {
            Ok(event) => {
                let params = serde_json::to_value(&event).unwrap_or_default();
                let line = json_rpc_notification(AGENT_EVENT_NOTIFY_METHOD, params);
                writer.send(line).await;
            }
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(
                    microapp = %microapp_id,
                    lagged = n,
                    "agent_event subscriber lagged; microapp should re-issue agent_events/read",
                );
            }
            Err(RecvError::Closed) => {
                tracing::debug!(
                    microapp = %microapp_id,
                    "agent_event broadcast closed; subscriber exiting",
                );
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn empty_extensions_cfg() -> ExtensionsConfig {
        // Reuse the type's Default by deserializing the canonical
        // empty doc (`enabled: false`). Avoids hard-coding every
        // field which would drift if upstream adds knobs.
        serde_yaml::from_str("enabled: false").expect("yaml")
    }

    fn extensions_cfg_with_grant(id: &str, caps: &[&str]) -> ExtensionsConfig {
        let mut cfg = empty_extensions_cfg();
        let entry = nexo_config::types::extensions::ExtensionEntry {
            capabilities_grant: caps.iter().map(|s| s.to_string()).collect(),
            allow_external_bind: false,
        };
        cfg.entries.insert(id.to_string(), entry);
        cfg
    }

    fn admin_caps(required: &[&str], optional: &[&str]) -> AdminCapabilities {
        AdminCapabilities {
            required: required.iter().map(|s| s.to_string()).collect(),
            optional: optional.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn noop_reload() -> ReloadSignal {
        Arc::new(|| {})
    }

    #[tokio::test]
    async fn build_returns_none_when_no_admin_caps_declared() {
        let cfg = empty_extensions_cfg();
        let manifests: BTreeMap<String, AdminCapabilities> = BTreeMap::new();
        let dir = tempfile::tempdir().unwrap();
        let result = AdminRpcBootstrap::build(AdminBootstrapInputs {
            config_dir: dir.path(),
            secrets_root: dir.path(),
            audit_db: None,
            extensions_cfg: &cfg,
            admin_capabilities: &manifests,
            http_server_capabilities: &BTreeMap::new(),
            reload_signal: noop_reload(),
            transcript_reader: None,
            broker: None,
            transcript_writer: None,
            processing_store: None,
            tenant_store: None,
            mcp_store: None,
            plugin_doctor: None,
            plugin_handles_cell: None,
            persona_install_roots: Vec::new(),
            plugin_restarter: None,
            plugin_installer: None,
            plugin_discovery: None,
            memory_reader: None,
            memory_snapshot_reader: None,
            secrets_store: None,
            llm_provider_probe: None,
            llm_completer: None,
            llm_provider_catalog: Vec::new(),
            auth_rotator: None,
            auth_token_path: None,
            auth_initial_hash: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
            persisters: Vec::new(),
            pairing_triggers: Default::default(),
            plugin_admin_router: None,
        })
        .await
        .unwrap();
        assert!(result.is_none(), "no admin wires when no caps declared");
    }

    #[tokio::test]
    async fn build_fails_when_required_capability_not_granted() {
        let cfg = extensions_cfg_with_grant("agent-creator", &[]);
        let mut manifests = BTreeMap::new();
        manifests.insert("agent-creator".into(), admin_caps(&["agents_crud"], &[]));
        let dir = tempfile::tempdir().unwrap();
        let err = AdminRpcBootstrap::build(AdminBootstrapInputs {
            config_dir: dir.path(),
            secrets_root: dir.path(),
            audit_db: None,
            extensions_cfg: &cfg,
            admin_capabilities: &manifests,
            http_server_capabilities: &BTreeMap::new(),
            reload_signal: noop_reload(),
            transcript_reader: None,
            broker: None,
            transcript_writer: None,
            processing_store: None,
            tenant_store: None,
            mcp_store: None,
            plugin_doctor: None,
            plugin_handles_cell: None,
            persona_install_roots: Vec::new(),
            plugin_restarter: None,
            plugin_installer: None,
            plugin_discovery: None,
            memory_reader: None,
            memory_snapshot_reader: None,
            secrets_store: None,
            llm_provider_probe: None,
            llm_completer: None,
            llm_provider_catalog: Vec::new(),
            auth_rotator: None,
            auth_token_path: None,
            auth_initial_hash: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
            persisters: Vec::new(),
            pairing_triggers: Default::default(),
            plugin_admin_router: None,
        })
        .await
        .unwrap_err();
        let detail = format!("{err}");
        assert!(detail.contains("agents_crud") || detail.contains("RequiredNotGranted"));
    }

    #[tokio::test]
    async fn build_succeeds_and_routes_for_granted_microapp() {
        let cfg = extensions_cfg_with_grant("agent-creator", &["agents_crud"]);
        let mut manifests = BTreeMap::new();
        manifests.insert("agent-creator".into(), admin_caps(&["agents_crud"], &[]));
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = AdminRpcBootstrap::build(AdminBootstrapInputs {
            config_dir: dir.path(),
            secrets_root: dir.path(),
            audit_db: None,
            extensions_cfg: &cfg,
            admin_capabilities: &manifests,
            http_server_capabilities: &BTreeMap::new(),
            reload_signal: noop_reload(),
            transcript_reader: None,
            broker: None,
            transcript_writer: None,
            processing_store: None,
            tenant_store: None,
            mcp_store: None,
            plugin_doctor: None,
            plugin_handles_cell: None,
            persona_install_roots: Vec::new(),
            plugin_restarter: None,
            plugin_installer: None,
            plugin_discovery: None,
            memory_reader: None,
            memory_snapshot_reader: None,
            secrets_store: None,
            llm_provider_probe: None,
            llm_completer: None,
            llm_provider_catalog: Vec::new(),
            auth_rotator: None,
            auth_token_path: None,
            auth_initial_hash: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
            persisters: Vec::new(),
            pairing_triggers: Default::default(),
            plugin_admin_router: None,
        })
        .await
        .unwrap()
        .expect("admin wire built");

        assert!(bootstrap.is_active());
        assert_eq!(bootstrap.wired_ids(), vec!["agent-creator".to_string()]);
        let opts = bootstrap
            .spawn_options_for("agent-creator", StdioSpawnOptions::default())
            .expect("opts for granted microapp");
        assert!(opts.admin_router.is_some());
        // Unrelated microapp gets None back so the host falls back.
        assert!(bootstrap
            .spawn_options_for("other", StdioSpawnOptions::default())
            .is_none());
    }

    #[tokio::test]
    async fn subscribe_task_spawned_for_microapp_with_transcripts_subscribe() {
        let cfg =
            extensions_cfg_with_grant("agent-creator", &["agents_crud", "transcripts_subscribe"]);
        let mut manifests = BTreeMap::new();
        manifests.insert(
            "agent-creator".into(),
            admin_caps(&["agents_crud"], &["transcripts_subscribe"]),
        );
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = AdminRpcBootstrap::build_with_firehose(
            AdminBootstrapInputs {
                config_dir: dir.path(),
                secrets_root: dir.path(),
                audit_db: None,
                extensions_cfg: &cfg,
                admin_capabilities: &manifests,
                http_server_capabilities: &BTreeMap::new(),
                reload_signal: noop_reload(),
                transcript_reader: None,
                broker: None,
                transcript_writer: None,
                processing_store: None,
                tenant_store: None,
                mcp_store: None,
                plugin_doctor: None,
                plugin_handles_cell: None,
                persona_install_roots: Vec::new(),
                plugin_restarter: None,
                plugin_installer: None,
                plugin_discovery: None,
                memory_reader: None,
                memory_snapshot_reader: None,
                secrets_store: None,
                llm_provider_probe: None,
                llm_completer: None,
                llm_provider_catalog: Vec::new(),
                auth_rotator: None,
                auth_token_path: None,
                auth_initial_hash: None,
                skills_store: None,
                escalation_store: None,
                agent_event_log: None,
                persisters: Vec::new(),
                pairing_triggers: Default::default(),
                plugin_admin_router: None,
            },
            true,
        )
        .await
        .unwrap()
        .expect("admin wire built");
        assert_eq!(
            bootstrap.subscribe_task_count(),
            1,
            "one subscribe task for the granted microapp",
        );
    }

    #[tokio::test]
    async fn no_subscribe_task_without_subscribe_capability() {
        let cfg = extensions_cfg_with_grant("agent-creator", &["agents_crud"]);
        let mut manifests = BTreeMap::new();
        manifests.insert("agent-creator".into(), admin_caps(&["agents_crud"], &[]));
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = AdminRpcBootstrap::build_with_firehose(
            AdminBootstrapInputs {
                config_dir: dir.path(),
                secrets_root: dir.path(),
                audit_db: None,
                extensions_cfg: &cfg,
                admin_capabilities: &manifests,
                http_server_capabilities: &BTreeMap::new(),
                reload_signal: noop_reload(),
                transcript_reader: None,
                broker: None,
                transcript_writer: None,
                processing_store: None,
                tenant_store: None,
                mcp_store: None,
                plugin_doctor: None,
                plugin_handles_cell: None,
                persona_install_roots: Vec::new(),
                plugin_restarter: None,
                plugin_installer: None,
                plugin_discovery: None,
                memory_reader: None,
                memory_snapshot_reader: None,
                secrets_store: None,
                llm_provider_probe: None,
                llm_completer: None,
                llm_provider_catalog: Vec::new(),
                auth_rotator: None,
                auth_token_path: None,
                auth_initial_hash: None,
                skills_store: None,
                escalation_store: None,
                agent_event_log: None,
                persisters: Vec::new(),
                pairing_triggers: Default::default(),
                plugin_admin_router: None,
            },
            true,
        )
        .await
        .unwrap()
        .expect("admin wire built");
        assert_eq!(bootstrap.subscribe_task_count(), 0);
    }

    #[tokio::test]
    async fn agent_events_subscribe_all_also_spawns_task() {
        let cfg =
            extensions_cfg_with_grant("audit-app", &["agents_crud", "agent_events_subscribe_all"]);
        let mut manifests = BTreeMap::new();
        manifests.insert(
            "audit-app".into(),
            admin_caps(&["agents_crud"], &["agent_events_subscribe_all"]),
        );
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = AdminRpcBootstrap::build_with_firehose(
            AdminBootstrapInputs {
                config_dir: dir.path(),
                secrets_root: dir.path(),
                audit_db: None,
                extensions_cfg: &cfg,
                admin_capabilities: &manifests,
                http_server_capabilities: &BTreeMap::new(),
                reload_signal: noop_reload(),
                transcript_reader: None,
                broker: None,
                transcript_writer: None,
                processing_store: None,
                tenant_store: None,
                mcp_store: None,
                plugin_doctor: None,
                plugin_handles_cell: None,
                persona_install_roots: Vec::new(),
                plugin_restarter: None,
                plugin_installer: None,
                plugin_discovery: None,
                memory_reader: None,
                memory_snapshot_reader: None,
                secrets_store: None,
                llm_provider_probe: None,
                llm_completer: None,
                llm_provider_catalog: Vec::new(),
                auth_rotator: None,
                auth_token_path: None,
                auth_initial_hash: None,
                skills_store: None,
                escalation_store: None,
                agent_event_log: None,
                persisters: Vec::new(),
                pairing_triggers: Default::default(),
                plugin_admin_router: None,
            },
            true,
        )
        .await
        .unwrap()
        .expect("admin wire built");
        assert_eq!(bootstrap.subscribe_task_count(), 1);
    }

    #[tokio::test]
    async fn external_bind_without_opt_in_fails_boot() {
        use nexo_plugin_manifest::manifest::HttpServerCapability;
        let cfg = extensions_cfg_with_grant("agent-creator", &["agents_crud"]);
        let mut manifests = BTreeMap::new();
        manifests.insert("agent-creator".into(), admin_caps(&["agents_crud"], &[]));
        let mut http: BTreeMap<String, HttpServerCapability> = BTreeMap::new();
        http.insert(
            "agent-creator".into(),
            HttpServerCapability {
                port: 9001,
                bind: "0.0.0.0".into(),
                token_env: "T".into(),
                health_path: "/healthz".into(),
                extra_env_passthrough: Vec::new(),
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let err = AdminRpcBootstrap::build_with_firehose(
            AdminBootstrapInputs {
                config_dir: dir.path(),
                secrets_root: dir.path(),
                audit_db: None,
                extensions_cfg: &cfg,
                admin_capabilities: &manifests,
                http_server_capabilities: &http,
                reload_signal: noop_reload(),
                transcript_reader: None,
                broker: None,
                transcript_writer: None,
                processing_store: None,
                tenant_store: None,
                mcp_store: None,
                plugin_doctor: None,
                plugin_handles_cell: None,
                persona_install_roots: Vec::new(),
                plugin_restarter: None,
                plugin_installer: None,
                plugin_discovery: None,
                memory_reader: None,
                memory_snapshot_reader: None,
                secrets_store: None,
                llm_provider_probe: None,
                llm_completer: None,
                llm_provider_catalog: Vec::new(),
                auth_rotator: None,
                auth_token_path: None,
                auth_initial_hash: None,
                skills_store: None,
                escalation_store: None,
                agent_event_log: None,
                persisters: Vec::new(),
                pairing_triggers: Default::default(),
                plugin_admin_router: None,
            },
            true,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, AdminBootstrapError::ExternalBindNotAllowed { .. }),
            "got: {err:?}",
        );
    }

    #[tokio::test]
    async fn external_bind_with_opt_in_passes_boot() {
        use nexo_plugin_manifest::manifest::HttpServerCapability;
        let mut cfg = extensions_cfg_with_grant("agent-creator", &["agents_crud"]);
        // Flip the opt-in flag.
        if let Some(entry) = cfg.entries.get_mut("agent-creator") {
            entry.allow_external_bind = true;
        }
        let mut manifests = BTreeMap::new();
        manifests.insert("agent-creator".into(), admin_caps(&["agents_crud"], &[]));
        let mut http: BTreeMap<String, HttpServerCapability> = BTreeMap::new();
        http.insert(
            "agent-creator".into(),
            HttpServerCapability {
                port: 9001,
                bind: "0.0.0.0".into(),
                token_env: "T".into(),
                health_path: "/healthz".into(),
                extra_env_passthrough: Vec::new(),
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = AdminRpcBootstrap::build_with_firehose(
            AdminBootstrapInputs {
                config_dir: dir.path(),
                secrets_root: dir.path(),
                audit_db: None,
                extensions_cfg: &cfg,
                admin_capabilities: &manifests,
                http_server_capabilities: &http,
                reload_signal: noop_reload(),
                transcript_reader: None,
                broker: None,
                transcript_writer: None,
                processing_store: None,
                tenant_store: None,
                mcp_store: None,
                plugin_doctor: None,
                plugin_handles_cell: None,
                persona_install_roots: Vec::new(),
                plugin_restarter: None,
                plugin_installer: None,
                plugin_discovery: None,
                memory_reader: None,
                memory_snapshot_reader: None,
                secrets_store: None,
                llm_provider_probe: None,
                llm_completer: None,
                llm_provider_catalog: Vec::new(),
                auth_rotator: None,
                auth_token_path: None,
                auth_initial_hash: None,
                skills_store: None,
                escalation_store: None,
                agent_event_log: None,
                persisters: Vec::new(),
                pairing_triggers: Default::default(),
                plugin_admin_router: None,
            },
            true,
        )
        .await
        .unwrap()
        .expect("bootstrap built");
        assert!(bootstrap.is_active());
    }

    #[tokio::test]
    async fn loopback_bind_never_requires_opt_in() {
        use nexo_plugin_manifest::manifest::HttpServerCapability;
        let cfg = extensions_cfg_with_grant("agent-creator", &["agents_crud"]);
        let mut manifests = BTreeMap::new();
        manifests.insert("agent-creator".into(), admin_caps(&["agents_crud"], &[]));
        let mut http: BTreeMap<String, HttpServerCapability> = BTreeMap::new();
        http.insert(
            "agent-creator".into(),
            HttpServerCapability {
                port: 9001,
                bind: "127.0.0.1".into(),
                token_env: "T".into(),
                health_path: "/healthz".into(),
                extra_env_passthrough: Vec::new(),
            },
        );
        let dir = tempfile::tempdir().unwrap();
        // allow_external_bind stays false; should still pass.
        let _ = AdminRpcBootstrap::build_with_firehose(
            AdminBootstrapInputs {
                config_dir: dir.path(),
                secrets_root: dir.path(),
                audit_db: None,
                extensions_cfg: &cfg,
                admin_capabilities: &manifests,
                http_server_capabilities: &http,
                reload_signal: noop_reload(),
                transcript_reader: None,
                broker: None,
                transcript_writer: None,
                processing_store: None,
                tenant_store: None,
                mcp_store: None,
                plugin_doctor: None,
                plugin_handles_cell: None,
                persona_install_roots: Vec::new(),
                plugin_restarter: None,
                plugin_installer: None,
                plugin_discovery: None,
                memory_reader: None,
                memory_snapshot_reader: None,
                secrets_store: None,
                llm_provider_probe: None,
                llm_completer: None,
                llm_provider_catalog: Vec::new(),
                auth_rotator: None,
                auth_token_path: None,
                auth_initial_hash: None,
                skills_store: None,
                escalation_store: None,
                agent_event_log: None,
                persisters: Vec::new(),
                pairing_triggers: Default::default(),
                plugin_admin_router: None,
            },
            true,
        )
        .await
        .expect("bootstrap built");
    }

    /// Boot must share the SAME
    /// `Arc<dyn ProcessingControlStore>` between the admin RPC
    /// dispatcher and the agent runtime. Without sharing, a
    /// pause RPC would land on one store while the runtime
    /// would consult a different one, and pause would never
    /// reach the inbound loop.
    #[tokio::test]
    async fn shared_processing_store_round_trips_pause_to_runtime_check() {
        use crate::admin_adapters::InMemoryProcessingControlStore;
        use nexo_core::agent::admin_rpc::domains::processing::ProcessingControlStore;
        use nexo_tool_meta::admin::processing::{ProcessingControlState, ProcessingScope};

        let cfg = extensions_cfg_with_grant("agent-creator", &["agents_crud"]);
        let mut manifests = BTreeMap::new();
        manifests.insert("agent-creator".into(), admin_caps(&["agents_crud"], &[]));
        let dir = tempfile::tempdir().unwrap();

        // ONE store, two consumers.
        let shared: Arc<dyn ProcessingControlStore> =
            Arc::new(InMemoryProcessingControlStore::new());

        let _bootstrap = AdminRpcBootstrap::build(AdminBootstrapInputs {
            config_dir: dir.path(),
            secrets_root: dir.path(),
            audit_db: None,
            extensions_cfg: &cfg,
            admin_capabilities: &manifests,
            http_server_capabilities: &BTreeMap::new(),
            reload_signal: noop_reload(),
            transcript_reader: None,
            broker: None,
            transcript_writer: None,
            processing_store: Some(shared.clone()),
            tenant_store: None,
            mcp_store: None,
            plugin_doctor: None,
            plugin_handles_cell: None,
            persona_install_roots: Vec::new(),
            plugin_restarter: None,
            plugin_installer: None,
            plugin_discovery: None,
            memory_reader: None,
            memory_snapshot_reader: None,
            secrets_store: None,
            llm_provider_probe: None,
            llm_completer: None,
            llm_provider_catalog: Vec::new(),
            auth_rotator: None,
            auth_token_path: None,
            auth_initial_hash: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
            persisters: Vec::new(),
            pairing_triggers: Default::default(),
            plugin_admin_router: None,
        })
        .await
        .unwrap()
        .expect("admin wire built with shared store");

        // Simulate: admin RPC pauses a scope (operator action).
        let scope = ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "wa.0".into(),
            contact_id: "wa.55".into(),
            mcp_channel_source: None,
        };
        shared
            .set(
                scope.clone(),
                ProcessingControlState::PausedByOperator {
                    scope: scope.clone(),
                    paused_at_ms: 1_700_000_000_000,
                    operator_token_hash: "h".into(),
                    reason: None,
                },
            )
            .await
            .unwrap();

        // The runtime would receive the SAME `Arc` via
        // `Runtime::with_processing_store(shared.clone())`.
        // Verify the runtime side reads the paused state
        // (same instance — pause is visible).
        let runtime_view: Arc<dyn ProcessingControlStore> = shared.clone();
        let read = runtime_view.get(&scope).await.unwrap();
        assert!(
            matches!(read, ProcessingControlState::PausedByOperator { .. }),
            "runtime side did not see the pause set via admin RPC: {read:?}",
        );
    }

    #[tokio::test]
    async fn firehose_off_silences_subscribe_tasks_but_keeps_admin_rpc() {
        let cfg =
            extensions_cfg_with_grant("agent-creator", &["agents_crud", "transcripts_subscribe"]);
        let mut manifests = BTreeMap::new();
        manifests.insert(
            "agent-creator".into(),
            admin_caps(&["agents_crud"], &["transcripts_subscribe"]),
        );
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = AdminRpcBootstrap::build_with_firehose(
            AdminBootstrapInputs {
                config_dir: dir.path(),
                secrets_root: dir.path(),
                audit_db: None,
                extensions_cfg: &cfg,
                admin_capabilities: &manifests,
                http_server_capabilities: &BTreeMap::new(),
                reload_signal: noop_reload(),
                transcript_reader: None,
                broker: None,
                transcript_writer: None,
                processing_store: None,
                tenant_store: None,
                mcp_store: None,
                plugin_doctor: None,
                plugin_handles_cell: None,
                persona_install_roots: Vec::new(),
                plugin_restarter: None,
                plugin_installer: None,
                plugin_discovery: None,
                memory_reader: None,
                memory_snapshot_reader: None,
                secrets_store: None,
                llm_provider_probe: None,
                llm_completer: None,
                llm_provider_catalog: Vec::new(),
                auth_rotator: None,
                auth_token_path: None,
                auth_initial_hash: None,
                skills_store: None,
                escalation_store: None,
                agent_event_log: None,
                persisters: Vec::new(),
                pairing_triggers: Default::default(),
                plugin_admin_router: None,
            },
            false,
        )
        .await
        .unwrap()
        .expect("admin wire built");
        // INVENTORY toggle silences the firehose but the admin
        // dispatcher (CRUD + agent_events backfill) is still
        // wired — microapp keeps its router for direct calls.
        assert_eq!(bootstrap.subscribe_task_count(), 0);
        assert!(bootstrap.is_active());
    }

    #[tokio::test]
    async fn agent_event_log_when_wired_durably_captures_emissions() {
        // Boot composes Tee([Broadcast, SqliteAgentEventLog]) so
        // every emit reaches both the live broadcast (microapp
        // notifications) AND the durable log (operator-dashboard
        // backfill across daemon restart).
        use nexo_core::agent::admin_rpc::{
            AgentEventLog, AgentEventLogFilter, SqliteAgentEventLog,
        };
        use nexo_tool_meta::admin::agent_events::{AgentEventKind, TranscriptRole};
        use uuid::Uuid;

        let cfg = extensions_cfg_with_grant("agent-creator", &["agents_crud"]);
        let mut manifests = BTreeMap::new();
        manifests.insert("agent-creator".into(), admin_caps(&["agents_crud"], &[]));
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(SqliteAgentEventLog::open_memory().await.unwrap());
        let bootstrap = AdminRpcBootstrap::build_with_firehose(
            AdminBootstrapInputs {
                config_dir: dir.path(),
                secrets_root: dir.path(),
                audit_db: None,
                extensions_cfg: &cfg,
                admin_capabilities: &manifests,
                http_server_capabilities: &BTreeMap::new(),
                reload_signal: noop_reload(),
                transcript_reader: None,
                broker: None,
                transcript_writer: None,
                processing_store: None,
                tenant_store: None,
                mcp_store: None,
                plugin_doctor: None,
                plugin_handles_cell: None,
                persona_install_roots: Vec::new(),
                plugin_restarter: None,
                plugin_installer: None,
                plugin_discovery: None,
                memory_reader: None,
                memory_snapshot_reader: None,
                secrets_store: None,
                llm_provider_probe: None,
                llm_completer: None,
                llm_provider_catalog: Vec::new(),
                auth_rotator: None,
                auth_token_path: None,
                auth_initial_hash: None,
                skills_store: None,
                escalation_store: None,
                agent_event_log: Some(log.clone()),
                persisters: Vec::new(),
                pairing_triggers: Default::default(),
                plugin_admin_router: None,
            },
            true,
        )
        .await
        .unwrap()
        .expect("admin wire built");

        // Drive an emit through the boot-built emitter.
        bootstrap
            .event_emitter()
            .emit(AgentEventKind::TranscriptAppended {
                agent_id: "ana".into(),
                session_id: Uuid::nil(),
                seq: 0,
                role: TranscriptRole::User,
                body: "hola".into(),
                sent_at_ms: 1_700_000_000_000,
                sender_id: None,
                source_plugin: "whatsapp".into(),
                tenant_id: None,
            })
            .await;

        // Durable side captured the row.
        let rows = log
            .list_recent(&AgentEventLogFilter {
                agent_id: "ana".into(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "Tee[Broadcast, Log] persists emit");
    }

    // ── domain kill-switch coverage ─────

    fn make_grants(
        items: &[(&str, &[&str])],
    ) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
        items
            .iter()
            .map(|(id, caps)| {
                (
                    (*id).to_string(),
                    caps.iter().map(|c| (*c).to_string()).collect(),
                )
            })
            .collect()
    }

    /// Empty env — every capability passes through. Verifies the
    /// happy path doesn't accidentally strip grants in the
    /// absence of any kill-switch env.
    #[test]
    fn kill_switches_unset_passes_grants_through() {
        let mut grants = make_grants(&[
            ("admin", &["agents_crud", "skills_crud", "memory_snapshot"]),
            ("creator", &["channels_crud"]),
        ]);
        let stripped = apply_admin_domain_kill_switches(&mut grants, |_| None);
        assert!(stripped.is_empty());
        assert!(grants["admin"].contains("agents_crud"));
        assert!(grants["admin"].contains("skills_crud"));
        assert!(grants["creator"].contains("channels_crud"));
    }

    /// Env values like "1" / "true" / unrelated text mean ON.
    /// Only the explicit "off"-ish set strips.
    #[test]
    fn kill_switches_truthy_value_passes_grants_through() {
        let mut grants = make_grants(&[("admin", &["agents_crud"])]);
        let stripped = apply_admin_domain_kill_switches(&mut grants, |k| {
            if k == "NEXO_MICROAPP_ADMIN_AGENTS_ENABLED" {
                Some("1".into())
            } else {
                None
            }
        });
        assert!(stripped.is_empty());
        assert!(grants["admin"].contains("agents_crud"));
    }

    /// Single domain off → matching capability stripped, others intact.
    #[test]
    fn kill_switches_single_domain_off_strips_one_cap() {
        let mut grants =
            make_grants(&[("admin", &["agents_crud", "skills_crud", "channels_crud"])]);
        let stripped = apply_admin_domain_kill_switches(&mut grants, |k| {
            if k == "NEXO_MICROAPP_ADMIN_AGENTS_ENABLED" {
                Some("0".into())
            } else {
                None
            }
        });
        assert_eq!(stripped.len(), 1);
        assert_eq!(stripped[0].0, "admin");
        assert_eq!(stripped[0].1, "agents_crud");
        assert_eq!(stripped[0].2, "NEXO_MICROAPP_ADMIN_AGENTS_ENABLED");
        assert!(!grants["admin"].contains("agents_crud"));
        assert!(grants["admin"].contains("skills_crud"));
        assert!(grants["admin"].contains("channels_crud"));
    }

    /// Multiple domains off → all matching caps stripped from
    /// every microapp; unrelated grants survive.
    #[test]
    fn kill_switches_multiple_domains_off_strip_all_matches() {
        let mut grants = make_grants(&[
            ("admin", &["agents_crud", "skills_crud", "memory_snapshot"]),
            ("creator", &["agents_crud", "channels_crud"]),
        ]);
        let stripped = apply_admin_domain_kill_switches(&mut grants, |k| match k {
            "NEXO_MICROAPP_ADMIN_AGENTS_ENABLED" | "NEXO_MICROAPP_ADMIN_SKILLS_ENABLED" => {
                Some("0".into())
            }
            _ => None,
        });
        assert_eq!(
            stripped.len(),
            3,
            "expected 3 stripped (admin x2 + creator x1)"
        );
        assert!(!grants["admin"].contains("agents_crud"));
        assert!(!grants["admin"].contains("skills_crud"));
        assert!(grants["admin"].contains("memory_snapshot"));
        assert!(!grants["creator"].contains("agents_crud"));
        assert!(grants["creator"].contains("channels_crud"));
    }

    /// All "off" variants ("0", "false", "FALSE", "off", "OFF")
    /// are recognised. Empty + whitespace + arbitrary truthy
    /// strings stay ON.
    #[test]
    fn kill_switches_off_value_aliases() {
        for v in ["0", "false", "FALSE", "off", "OFF"] {
            let mut grants = make_grants(&[("admin", &["secrets_write"])]);
            let stripped = apply_admin_domain_kill_switches(&mut grants, |k| {
                if k == "NEXO_MICROAPP_ADMIN_SECRETS_ENABLED" {
                    Some(v.into())
                } else {
                    None
                }
            });
            assert_eq!(stripped.len(), 1, "value `{v}` must strip");
        }
        for v in ["", "  ", "1", "true", "yes", "anything"] {
            let mut grants = make_grants(&[("admin", &["secrets_write"])]);
            let stripped = apply_admin_domain_kill_switches(&mut grants, |k| {
                if k == "NEXO_MICROAPP_ADMIN_SECRETS_ENABLED" {
                    Some(v.into())
                } else {
                    None
                }
            });
            assert!(
                stripped.is_empty(),
                "value `{v}` must NOT strip (only explicit off-ish strip)"
            );
        }
    }

    /// Stripping a capability the microapp never had is a no-op
    /// (no spurious "stripped" entries).
    #[test]
    fn kill_switches_no_op_when_grant_absent() {
        let mut grants = make_grants(&[("admin", &["channels_crud"])]);
        let stripped = apply_admin_domain_kill_switches(&mut grants, |k| {
            if k == "NEXO_MICROAPP_ADMIN_AGENTS_ENABLED" {
                Some("0".into())
            } else {
                None
            }
        });
        assert!(
            stripped.is_empty(),
            "agents kill switch must NOT touch a microapp without agents_crud grant"
        );
        assert!(grants["admin"].contains("channels_crud"));
    }

    /// All 9 capability → env-var mappings are present in
    /// `ADMIN_DOMAIN_KILL_SWITCHES`. Catches a future drop /
    /// rename via mismatch with the INVENTORY.
    #[test]
    fn kill_switches_inventory_covers_9_capabilities() {
        let caps: std::collections::HashSet<&str> =
            ADMIN_DOMAIN_KILL_SWITCHES.iter().map(|(c, _)| *c).collect();
        for expected in [
            "agents_crud",
            "credentials_crud",
            "pairing_initiate",
            "llm_keys_crud",
            "channels_crud",
            "skills_crud",
            "tenants_crud",
            "secrets_write",
            "auth_rotate",
        ] {
            assert!(
                caps.contains(expected),
                "ADMIN_DOMAIN_KILL_SWITCHES must include `{expected}`"
            );
        }
        assert_eq!(caps.len(), 9, "exactly 9 capabilities are kill-switched");
    }
}
