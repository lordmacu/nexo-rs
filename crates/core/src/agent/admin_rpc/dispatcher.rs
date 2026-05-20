//! Admin RPC dispatcher core.
//!
//! Single entry point `AdminRpcDispatcher::dispatch(microapp_id,
//! method, params) -> AdminRpcResult` invoked by the microapp
//! transport adapter when a JSON-RPC frame with `app:` ID prefix
//! arrives. Returns the typed result/error pair; caller frames +
//! writes the response.
//!
//! Every dispatch consults the capability gate and writes one
//! audit row. Domain handlers
//! (agents/credentials/pairing/llm_providers/channels/...) are
//! registered via the `install_*` methods; an unconfigured domain
//! returns `-32601`.

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use thiserror::Error;

use super::audit::{
    hash_params, now_epoch_ms, AdminAuditReader, AdminAuditResult, AdminAuditRow, AdminAuditWriter,
    InMemoryAuditWriter,
};
use nexo_tool_meta::admin::agent_events::{AgentEventKind, PluginUiChangeKind};
use nexo_tool_meta::admin::plugin_discovery::TrustTier;

use super::capabilities::CapabilitySet;
use super::channel_outbound::ChannelOutboundDispatcher;
use super::domains::agent_events::TranscriptReader;
use super::domains::agents::YamlPatcher;
use super::domains::credentials::{ChannelCredentialPersister, CredentialStore, PersisterRegistry};
use super::domains::escalations::EscalationStore;
use super::domains::llm_providers::LlmYamlPatcher;
use super::domains::mcp::McpServerStore;
use super::domains::memory::{MemoryReader, MemorySnapshotReader};
use super::domains::pairing::{PairingChallengeStore, PairingNotifier};
use super::domains::pairing_channels::PairingChannelsReader;
use super::domains::persona::{PersonaSnapshotReader, PersonaStore};
use super::domains::plugin_discovery::DiscoveryReader;
use super::domains::plugin_doctor::PluginDoctorReader;
use super::domains::plugin_install::PluginInstaller;
use super::domains::plugin_restart::PluginRestarter;
use super::domains::processing::ProcessingControlStore;
use super::domains::skills::SkillsStore;
use super::domains::tenants::TenantStore;
use super::pairing_trigger::{PairingChannelTriggers, PairingHandle};
use super::transcript_appender::TranscriptAppender;
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Reload signal callback — invoked by domain handlers after
/// successful yaml mutations to trigger config hot-reload.
/// Production wiring passes a closure that calls
/// `ConfigReloadCoordinator::trigger_reload`.
pub type ReloadSignal = Arc<dyn Fn() + Send + Sync>;

/// Typed admin RPC errors returned to the SDK side, matching the
/// JSON-RPC error code conventions documented in the spec.
#[non_exhaustive]
#[derive(Debug, Error, PartialEq)]
pub enum AdminRpcError {
    /// `-32601` — method name not registered or disabled.
    #[error("method_not_found: {0}")]
    MethodNotFound(String),
    /// `-32602` — caller-supplied params failed validation.
    #[error("invalid_params: {0}")]
    InvalidParams(String),
    /// `-32602` with a typed `data` payload — caller-supplied
    /// params failed validation AND the handler has a structured
    /// error code (e.g. [`nexo_tool_meta::admin::llm_providers::LlmProviderError`]).
    /// SPA discriminates by `data.code` to render localised
    /// messages without parsing free-form `msg`.
    #[error("invalid_params: {msg}")]
    InvalidParamsWithData {
        /// Free-form fallback message.
        msg: String,
        /// Typed structured payload.
        data: Value,
    },
    /// `-32004` — operator did not grant `capability` to this
    /// microapp via `extensions.yaml.<id>.capabilities_grant`.
    #[error("capability_not_granted: {capability} for method {method}")]
    CapabilityNotGranted {
        /// Required capability name.
        capability: String,
        /// Method that was rejected.
        method: String,
        /// Microapp that requested.
        microapp_id: String,
    },
    /// `-32603` — internal error.
    #[error("internal: {0}")]
    Internal(String),
}

impl AdminRpcError {
    /// Map to JSON-RPC error code for the wire frame.
    pub fn code(&self) -> i32 {
        match self {
            AdminRpcError::MethodNotFound(_) => -32601,
            AdminRpcError::InvalidParams(_) => -32602,
            AdminRpcError::InvalidParamsWithData { .. } => -32602,
            AdminRpcError::CapabilityNotGranted { .. } => -32004,
            AdminRpcError::Internal(_) => -32603,
        }
    }

    /// Optional structured `data` field for the wire frame.
    pub fn data(&self) -> Option<Value> {
        match self {
            AdminRpcError::CapabilityNotGranted {
                capability,
                method,
                microapp_id,
            } => Some(serde_json::json!({
                "capability": capability,
                "microapp_id": microapp_id,
                "method": method,
            })),
            AdminRpcError::InvalidParamsWithData { data, .. } => Some(data.clone()),
            _ => None,
        }
    }
}

/// Dispatch result — the caller (microapp transport adapter)
/// frames it as `result` or `error`.
#[derive(Debug)]
pub struct AdminRpcResult {
    /// Successful payload. Mutually exclusive with `error`.
    pub result: Option<Value>,
    /// Error payload when dispatch failed.
    pub error: Option<AdminRpcError>,
}

impl AdminRpcResult {
    /// Build a success result.
    pub fn ok(value: Value) -> Self {
        Self {
            result: Some(value),
            error: None,
        }
    }

    /// Build an error result.
    pub fn err(e: AdminRpcError) -> Self {
        Self {
            result: None,
            error: Some(e),
        }
    }
}

/// Admin RPC dispatcher.
///
/// Routes `nexo/admin/<domain>/<method>` requests to handlers,
/// consults [`CapabilitySet`] for the operator-granted capability
/// before each call, writes one [`AdminAuditRow`] per dispatch.
#[derive(Clone)]
pub struct AdminRpcDispatcher {
    capabilities: Arc<CapabilitySet>,
    audit: Arc<dyn AdminAuditWriter>,
    /// Yaml mutation surface used by the
    /// `agents` domain handlers. `None` = domain unavailable
    /// (returns -32601 for `nexo/admin/agents/*`). Production
    /// wiring constructs the adapter from `nexo_setup::yaml_patch`.
    agents_yaml: Option<Arc<dyn YamlPatcher>>,
    /// Config reload trigger called after
    /// successful yaml mutations. `None` = no-op (for early-boot
    /// tests).
    reload_signal: Option<ReloadSignal>,
    /// Credential filesystem store. `None`
    /// disables `nexo/admin/credentials/*`.
    credential_store: Option<Arc<dyn CredentialStore>>,
    /// Per-channel credential persisters keyed
    /// by channel id. Empty by default. Channel plugins register
    /// themselves at boot via [`Self::register_persister`].
    /// `credentials/register` looks up by `input.channel`; absent
    /// = opaque-only path (back-compat).
    persisters: PersisterRegistry,
    /// Pairing challenge store. `None` disables
    /// `nexo/admin/pairing/*`.
    pairing_store: Option<Arc<dyn PairingChallengeStore>>,
    /// WhatsApp bot channel handle. `None` disables
    /// `nexo/admin/whatsapp/bot/*`.
    wa_bot_handle: super::wa_bot::WaBotHandleArc,
    /// Phase 81.33.b.real Stage 4 — generic plugin admin router.
    /// Plugins declaring `[plugin.admin] method_prefix = "..."`
    /// in their manifest register with this router; the
    /// dispatcher consults it BEFORE returning method-not-found
    /// so plugin admin methods get forwarded via broker JSON-RPC
    /// without needing a hardcoded `.with_<plugin>_handle()`
    /// builder. `None` = no plugin admin routes (or daemon
    /// running without subprocess plugins).
    plugin_admin_router: Option<std::sync::Arc<nexo_pairing::plugin_admin::PluginAdminRouter>>,
    /// Broker handle paired with `plugin_admin_router` so the
    /// dispatcher can issue the forward request. `None` when the
    /// router is also `None`.
    plugin_admin_broker: Option<nexo_broker::AnyBroker>,
    /// Phase 99.5 — plugin admin-UI domain
    /// (`nexo/admin/plugin_ui/{list,describe,config_set}`). `None`
    /// disables the domain (returns -32601). Production wires it in
    /// `main.rs` boot with the plugin registry + plugins-yaml store
    /// + credential store + broker forwarder + trust resolver.
    plugin_ui: Option<Arc<super::domains::plugin_ui::PluginUiDomain>>,
    /// Push channel for
    /// `nexo/notify/pairing_status_changed`. `None` = best-effort
    /// (poll only, notifications dropped).
    pairing_notifier: Option<Arc<dyn PairingNotifier>>,
    /// Per-channel pairing trigger registry. Empty
    /// map = `pairing/start` returns `channel not supported` for
    /// any channel (no garbage Pending row). Production wires
    /// `WhatsappPairingTrigger` (and future telegram-link, etc).
    pairing_triggers: PairingChannelTriggers,
    /// Registry of in-flight pairing tasks
    /// keyed by `challenge_id`. `pairing/cancel` (and TTL
    /// eviction) call `handle.abort()` to stop the underlying
    /// trigger task cleanly.
    pairing_handles: Arc<DashMap<Uuid, PairingHandle>>,
    /// Root cancel token. Per-trigger handles
    /// derive their own children via `child_token`. Aborting
    /// the root cancels every in-flight pairing (used at
    /// shutdown).
    pairing_cancel_root: CancellationToken,
    /// `llm.yaml` mutator. `None` disables
    /// `nexo/admin/llm_providers/*`.
    llm_yaml: Option<Arc<dyn LlmYamlPatcher>>,
    /// Snapshot of every LLM provider factory the daemon has
    /// registered (builtins + plugin-contributed). Drives the
    /// `nexo/admin/llm_providers/catalog` RPC. `None` disables
    /// the RPC.
    llm_provider_catalog:
        Option<Arc<Vec<nexo_tool_meta::admin::llm_providers::LlmProviderCatalogEntry>>>,
    /// Schema-driven `upsert` lookup. `None` keeps
    /// the legacy `api_key_env` / `api_key_secret_value` path
    /// active; with a lookup wired the handler also accepts the
    /// new `fields: BTreeMap` payload validated against the
    /// factory's declared `credential_schema`.
    llm_factory_schema: Option<Arc<dyn super::domains::llm_providers::FactorySchemaLookup>>,
    /// OAuth verifier store. `None` disables
    /// `nexo/admin/llm_providers/oauth_*` (the SPA falls back to
    /// the manual `oauth_bundle_import` paste path).
    oauth_verifier_store: Option<Arc<dyn nexo_llm_auth::VerifierStore>>,
    /// Transcripts read surface. `None` disables
    /// `nexo/admin/agent_events/*`.
    transcript_reader: Option<Arc<dyn TranscriptReader>>,
    /// Audit-log read surface. `None`
    /// disables `nexo/admin/microapp_audit/*`. Production wires
    /// the same `SqliteAdminAuditWriter` that handles writes
    /// (the type implements both `AdminAuditWriter` and
    /// `AdminAuditReader`).
    audit_reader: Option<Arc<dyn AdminAuditReader>>,
    /// Processing control store. `None` disables
    /// `nexo/admin/processing/*`.
    processing_store: Option<Arc<dyn ProcessingControlStore>>,
    /// Escalation store. `None` disables
    /// `nexo/admin/escalations/*`. When BOTH this and
    /// `processing_store` are configured, a `pause` call
    /// auto-flips any matching `Pending` escalation to
    /// `Resolved { OperatorTakeover }`.
    escalation_store: Option<Arc<dyn EscalationStore>>,
    /// Skills CRUD store. `None` disables
    /// `nexo/admin/skills/*`. Production wires
    /// `nexo_setup::admin_adapters::FsSkillsStore` against the
    /// existing `SkillLoader` filesystem layout.
    skills_store: Option<Arc<dyn SkillsStore>>,
    /// Channel-outbound dispatcher used by
    /// `processing/intervention` when the action is `Reply`.
    /// `None` keeps the wire surface alive (`-32601` style
    /// rejection) but operator replies fail with
    /// `channel_unavailable`. Production wires the multi-channel
    /// router adapter living in `nexo-setup`.
    channel_outbound: Option<Arc<dyn ChannelOutboundDispatcher>>,
    /// Multi-tenant SaaS registry. `None`
    /// disables `nexo/admin/tenants/*` (single-tenant
    /// deployments where there is no operator-level tenant
    /// management). Production wires
    /// `nexo_setup::admin_adapters::TenantsYamlPatcher`.
    tenant_store: Option<Arc<dyn TenantStore>>,
    /// MCP server registry CRUD. `None`
    /// disables `nexo/admin/mcp/*` (mcp.yaml management via
    /// CLI / direct edit only). Production wires
    /// `nexo_core::agent::admin_rpc::domains::mcp::McpYamlStore`.
    mcp_store: Option<Arc<dyn McpServerStore>>,
    /// Plugin doctor snapshot reader.
    /// `None` keeps `nexo/admin/plugins/doctor` returning a typed
    /// `plugins domain not configured` -32603.
    plugin_doctor: Option<Arc<dyn PluginDoctorReader>>,
    /// Plugin-driven pairing UI descriptor reader.
    /// `None` keeps `nexo/admin/pairing/channels` returning a typed
    /// `pairing channels domain not configured` -32603. Production
    /// wires `nexo_setup::admin_adapters::PairingChannelsReaderImpl`.
    pairing_channels_reader: Option<Arc<dyn PairingChannelsReader>>,
    /// Phase 81.31 — read-side of the multi-locale persona system.
    /// When `Some`, `agents/get` enriches the response with
    /// `persona_locales`. `None` keeps legacy single-locale behavior.
    persona_snapshot_reader: Option<Arc<dyn PersonaSnapshotReader>>,
    /// Phase 81.31 — write-side. `None` keeps
    /// `nexo/admin/persona/save_localized` returning typed
    /// `persona domain not configured` -32603.
    persona_store: Option<Arc<dyn PersonaStore>>,
    /// Read-only persona TEMPLATE catalog. `None` keeps
    /// `nexo/admin/personas/list` + `…/template_get` returning a
    /// typed `persona catalog domain not configured` -32603.
    /// Personas are install_root-backed templates the admin
    /// wizard renders as "use as template" — they are NEVER
    /// merged into `cfg.agents`.
    persona_catalog: Option<Arc<dyn super::domains::persona::PersonaCatalogReader>>,
    /// Manual plugin restart adapter.
    /// `None` keeps `nexo/admin/plugins/restart` returning a typed
    /// `plugin restart domain not configured` -32603. Production
    /// wires `nexo_setup::admin_adapters::LivePluginRestarter`.
    plugin_restarter: Option<Arc<dyn PluginRestarter>>,
    /// Phase 97.1 — runtime plugin install / scan / uninstall.
    /// `None` keeps `nexo/admin/plugins/{scan,install,uninstall}`
    /// returning typed `plugin install domain not configured`
    /// -32603. Production wires
    /// `nexo_setup::admin_adapters::LivePluginInstaller`.
    plugin_installer: Option<Arc<dyn PluginInstaller>>,
    /// Plugin discovery reader (Phase 98.10). Surfaces the
    /// `nexo/admin/plugins/{search,compat_check,refresh_index}`
    /// trio. `None` keeps the three verbs returning typed
    /// `plugin discovery domain not configured` -32603. Production
    /// wires `nexo_setup::discovery_adapter::DefaultDiscoveryAdapter`
    /// (98.11), which delegates to
    /// `nexo_plugin_discovery::client::DefaultDiscoveryClient`.
    plugin_discovery: Option<Arc<dyn DiscoveryReader>>,
    /// Long-term memory query reader.
    /// `None` keeps `nexo/admin/memory/query` returning the typed
    /// `memory domain not configured` -32603.
    memory_reader: Option<Arc<dyn MemoryReader>>,
    /// Snapshot list reader. `None`
    /// keeps `nexo/admin/memory/list_snapshots` returning the
    /// typed `memory snapshot domain not configured` -32603.
    memory_snapshot_reader: Option<Arc<dyn MemorySnapshotReader>>,
    /// Transcript appender used by
    /// `processing/intervention` (and later `processing/resume`)
    /// to stamp operator replies / summary / replayed inbounds
    /// onto the agent transcript. `None` keeps the wire surface
    /// alive — the channel send still happens but
    /// `ProcessingAck.transcript_stamped` reports `Some(false)`.
    /// Production wires
    /// `nexo_setup::admin_adapters::TranscriptWriterAppender`.
    transcript_appender: Option<Arc<dyn TranscriptAppender>>,
    /// Firehose emitter shared with the
    /// transcripts subsystem. `None` keeps the wire surface
    /// alive but `escalations/resolve` (and the auto-resolve on
    /// `processing/pause`) skip the
    /// `AgentEventKind::EscalationResolved` emit so subscribers
    /// fall back to polling. Production threads
    /// `AdminRpcBootstrap.event_emitter()` here.
    event_emitter: Option<Arc<dyn crate::agent::agent_events::AgentEventEmitter>>,
    /// Secrets store. `None` disables
    /// `nexo/admin/secrets/write` (returns `Internal` for that
    /// method). Production wires
    /// `nexo_setup::secrets_store::FsSecretsStore` rooted at
    /// `<state_root>/secrets/` (mode 0600 file write +
    /// `std::env::set_var` so existing LLM clients see the
    /// new value without a daemon restart).
    secrets_store: Option<Arc<dyn super::domains::secrets::SecretsStore>>,
    /// Daemon-side LLM provider probe. `None`
    /// disables `nexo/admin/llm_providers/probe` (returns
    /// `Internal`). Production wires
    /// `nexo_setup::llm_provider_probe::HttpLlmProviderProbe`
    /// against the existing `LlmYamlPatcher` so the probe
    /// reflects the same config agent traffic would resolve.
    llm_provider_probe: Option<Arc<dyn super::domains::llm_providers::LlmProvidersProbe>>,
    /// Runtime LLM completer. Pluggable so the
    /// binary can wire `nexo_setup::llm_completer::RegistryLlmCompleter`
    /// (production) and tests can swap in a mock that captures
    /// inputs without touching the network. `None` disables
    /// `nexo/admin/llm/complete` (returns `Internal`).
    llm_completer: Option<Arc<dyn super::domains::llm::LlmCompleter>>,
    /// Operator bearer rotator. `None` disables
    /// `nexo/admin/auth/rotate_token` (returns `Internal`).
    /// Production wires `nexo_setup::auth_rotator::FsAuthRotator`
    /// rooted at `<state_root>/secrets/operator_token.txt`,
    /// hooked to the SDK notification multicaster + the firehose
    /// audit emitter so a successful rotation lands the live
    /// `nexo/notify/token_rotated` AND the durable
    /// `AgentEventKind::SecurityEvent::TokenRotated` audit row.
    auth_rotator: Option<Arc<dyn super::domains::auth::AuthRotator>>,
}

impl std::fmt::Debug for AdminRpcDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminRpcDispatcher")
            .field("audit", &self.audit)
            .field("agents_yaml", &self.agents_yaml.is_some())
            .field("reload_signal", &self.reload_signal.is_some())
            .finish()
    }
}

impl Default for AdminRpcDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl AdminRpcDispatcher {
    /// Build a dispatcher with empty capability grants and an
    /// in-memory audit writer.
    pub fn new() -> Self {
        Self {
            capabilities: CapabilitySet::empty(),
            audit: Arc::new(InMemoryAuditWriter::new()),
            agents_yaml: None,
            reload_signal: None,
            credential_store: None,
            persisters: PersisterRegistry::new(),
            pairing_store: None,
            wa_bot_handle: None,
            pairing_notifier: None,
            pairing_triggers: PairingChannelTriggers::new(),
            pairing_handles: Arc::new(DashMap::new()),
            pairing_cancel_root: CancellationToken::new(),
            llm_yaml: None,
            llm_provider_catalog: None,
            llm_factory_schema: None,
            oauth_verifier_store: None,
            transcript_reader: None,
            audit_reader: None,
            processing_store: None,
            escalation_store: None,
            skills_store: None,
            channel_outbound: None,
            tenant_store: None,
            mcp_store: None,
            plugin_doctor: None,
            pairing_channels_reader: None,
            persona_snapshot_reader: None,
            persona_store: None,
            persona_catalog: None,
            plugin_restarter: None,
            plugin_installer: None,
            plugin_discovery: None,
            memory_reader: None,
            memory_snapshot_reader: None,
            transcript_appender: None,
            event_emitter: None,
            secrets_store: None,
            llm_provider_probe: None,
            llm_completer: None,
            auth_rotator: None,
            plugin_admin_router: None,
            plugin_admin_broker: None,
            plugin_ui: None,
        }
    }

    /// Install the secrets domain. Production
    /// passes `nexo_setup::secrets_store::FsSecretsStore::new(&state_root)`.
    /// Without one, `nexo/admin/secrets/write` returns
    /// `Internal("secrets domain not configured")`.
    pub fn with_secrets_domain(
        mut self,
        store: Arc<dyn super::domains::secrets::SecretsStore>,
    ) -> Self {
        self.secrets_store = Some(store);
        self
    }

    /// Install the LLM provider probe.
    /// Production passes
    /// `nexo_setup::llm_provider_probe::HttpLlmProviderProbe::new(...)`.
    /// Without one, `nexo/admin/llm_providers/probe` returns
    /// `Internal("llm_providers probe not configured")`.
    pub fn with_llm_provider_probe(
        mut self,
        probe: Arc<dyn super::domains::llm_providers::LlmProvidersProbe>,
    ) -> Self {
        self.llm_provider_probe = Some(probe);
        self
    }

    /// Runtime LLM completer. Wire at boot
    /// from `nexo_setup::llm_completer::RegistryLlmCompleter::new(
    /// registry, llm_cfg)`. Without one,
    /// `nexo/admin/llm/complete` returns
    /// `Internal("llm completer not configured")`.
    pub fn with_llm_completer(
        mut self,
        completer: Arc<dyn super::domains::llm::LlmCompleter>,
    ) -> Self {
        self.llm_completer = Some(completer);
        self
    }

    /// Install the operator-bearer rotator.
    /// Production passes
    /// `nexo_setup::auth_rotator::FsAuthRotator::new(...)`.
    /// Without one, `nexo/admin/auth/rotate_token` returns
    /// `Internal("auth rotator not configured")`.
    pub fn with_auth_rotator(
        mut self,
        rotator: Arc<dyn super::domains::auth::AuthRotator>,
    ) -> Self {
        self.auth_rotator = Some(rotator);
        self
    }

    /// Install the firehose emitter shared with
    /// the transcripts subsystem so escalation resolve
    /// transitions also reach `nexo/notify/agent_event`
    /// subscribers in real time. Without one, the resolve
    /// transition still happens; subscribers fall back to
    /// polling `escalations/list`.
    pub fn with_event_emitter(
        mut self,
        emitter: Arc<dyn crate::agent::agent_events::AgentEventEmitter>,
    ) -> Self {
        self.event_emitter = Some(emitter);
        self
    }

    /// Phase 99 `99.x.sse-multi-site-emit` — fire `PluginUiChanged`
    /// after a successful install / uninstall / enable / disable so
    /// admin shells refetch `plugin_ui/list` live (no manual reload).
    /// Best-effort: `trust_tier` / `admin_ui_present` are placeholders
    /// the subsequent `list` refetch resolves authoritatively (the
    /// dispatcher holds no trust resolver). No coalescer — these are
    /// discrete operator actions, not a burst source.
    async fn emit_plugin_ui_change(&self, plugin_id: &str, kind: PluginUiChangeKind) {
        let Some(em) = &self.event_emitter else {
            return;
        };
        if plugin_id.is_empty() {
            return;
        }
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        em.emit(AgentEventKind::PluginUiChanged {
            event_kind: kind,
            plugin_id: plugin_id.to_string(),
            admin_ui_present: true,
            trust_tier: TrustTier::Unverified,
            at_ms,
        })
        .await;
    }

    /// Replace the capability set. Boot wiring calls this once
    /// after [`super::validate_capabilities_at_boot`] returns OK.
    pub fn with_capabilities(mut self, capabilities: Arc<CapabilitySet>) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Replace the audit writer. Tests inject in-memory; SQLite
    /// writer is wired in production.
    pub fn with_audit_writer(mut self, writer: Arc<dyn AdminAuditWriter>) -> Self {
        self.audit = writer;
        self
    }

    /// Install the agents domain. Production
    /// passes a `YamlPatcher` adapter wrapping
    /// `nexo_setup::yaml_patch::*`.
    pub fn with_agents_domain(mut self, yaml: Arc<dyn YamlPatcher>, reload: ReloadSignal) -> Self {
        self.agents_yaml = Some(yaml);
        self.reload_signal = Some(reload);
        self
    }

    /// Install the credentials domain. Reuses
    /// the agents-domain `YamlPatcher` + `ReloadSignal` (must be
    /// installed first via `with_agents_domain`).
    pub fn with_credentials_domain(mut self, store: Arc<dyn CredentialStore>) -> Self {
        self.credential_store = Some(store);
        self
    }

    /// Register a per-channel credential
    /// persister. Channel plugins (telegram, email, whatsapp,
    /// future slack/discord) call this at boot to bridge
    /// `credentials/register` from the opaque payload to their
    /// own runtime state (yaml accounts list, secret file,
    /// in-memory store).
    ///
    /// Panics on duplicate registration for the same channel id
    /// — this is a boot-time configuration error, not a runtime
    /// path. Returns `&mut Self` (not consuming `self`) so boot
    /// wiring can register N persisters in a loop.
    pub fn register_persister(
        &mut self,
        persister: Arc<dyn ChannelCredentialPersister>,
    ) -> &mut Self {
        let channel = persister.channel().to_string();
        if self.persisters.contains_key(&channel) {
            panic!("duplicate ChannelCredentialPersister registration for channel `{channel}`");
        }
        self.persisters.insert(channel, persister);
        self
    }

    /// Read-only accessor for the persister
    /// registered against `channel`. Used by the
    /// `credentials/register` + `credentials/revoke` handlers to
    /// route to the per-channel bridge. `None` = opaque-only
    /// fallback path.
    pub fn persister_for(&self, channel: &str) -> Option<Arc<dyn ChannelCredentialPersister>> {
        self.persisters.get(channel).cloned()
    }

    /// Install the pairing domain. `notifier`
    /// is optional; `None` keeps polling functional but skips
    /// `nexo/notify/pairing_status_changed` pushes.
    pub fn with_pairing_domain(
        mut self,
        store: Arc<dyn PairingChallengeStore>,
        notifier: Option<Arc<dyn PairingNotifier>>,
    ) -> Self {
        self.pairing_store = Some(store);
        self.pairing_notifier = notifier;
        self
    }

    /// Install the WhatsApp bot channel handle that backs
    /// `nexo/admin/whatsapp/bot/{list,send}`. Pass the
    /// implementation owned by the WhatsApp plugin at boot. Calling
    /// this with `None` (default) disables the routes.
    /// Phase 81.33.b.real Stage 4 — install the generic plugin
    /// admin router so manifest-declared admin methods get
    /// forwarded via broker JSON-RPC. Plugins opt in by adding
    /// `[plugin.admin] method_prefix = "..."` to their manifest;
    /// the daemon iterates plugin handles at boot and registers
    /// every declaration. The router is consulted BEFORE the
    /// dispatcher returns method-not-found, so plugin admin
    /// commands work without needing a per-plugin
    /// `.with_<plugin>_handle()` builder method.
    pub fn with_plugin_admin_router(
        mut self,
        router: std::sync::Arc<nexo_pairing::plugin_admin::PluginAdminRouter>,
        broker: nexo_broker::AnyBroker,
    ) -> Self {
        self.plugin_admin_router = Some(router);
        self.plugin_admin_broker = Some(broker);
        self
    }

    pub fn with_wa_bot_handle(mut self, handle: Arc<dyn super::wa_bot::WaBotHandle>) -> Self {
        self.wa_bot_handle = Some(handle);
        self
    }

    /// Phase 99.5 — install the plugin admin-UI domain. `None`
    /// (default) leaves `nexo/admin/plugin_ui/*` returning -32601.
    pub fn with_plugin_ui_domain(
        mut self,
        domain: Arc<super::domains::plugin_ui::PluginUiDomain>,
    ) -> Self {
        self.plugin_ui = Some(domain);
        self
    }

    /// Install per-channel pairing triggers.
    /// Empty map (default) = `pairing/start` rejects every
    /// `channel` with `channel not supported` (no garbage row).
    /// Production passes one entry per channel that has a
    /// configured plugin (WhatsApp + future telegram-link / etc).
    pub fn with_pairing_triggers(mut self, triggers: PairingChannelTriggers) -> Self {
        self.pairing_triggers = triggers;
        self
    }

    /// Observability hook: count of in-flight
    /// pairing handles. Used by health probes / TTL sweep.
    pub fn pairing_handles_len(&self) -> usize {
        self.pairing_handles.len()
    }

    /// Install the llm_providers domain.
    /// Production passes an `LlmYamlPatcher` adapter pointed at
    /// `llm.yaml`.
    pub fn with_llm_providers_domain(mut self, llm_yaml: Arc<dyn LlmYamlPatcher>) -> Self {
        self.llm_yaml = Some(llm_yaml);
        self
    }

    /// Install the LLM provider catalog snapshot. The vec is taken
    /// once at boot from `LlmRegistry::catalog()` and shared via
    /// `Arc` so the RPC handler can serialise it without cloning
    /// per-call. `None` keeps `nexo/admin/llm_providers/catalog`
    /// disabled.
    pub fn with_llm_provider_catalog(
        mut self,
        catalog: Vec<nexo_tool_meta::admin::llm_providers::LlmProviderCatalogEntry>,
    ) -> Self {
        self.llm_provider_catalog = Some(Arc::new(catalog));
        self
    }

    /// Install the schema lookup the upsert handler
    /// uses to validate operator payloads against the factory's
    /// declared `credential_schema`. `None` (default) keeps
    /// `llm_providers/upsert` on the legacy `api_key_env` path so
    /// older microapps don't break.
    pub fn with_llm_factory_schema(
        mut self,
        schema: Arc<dyn super::domains::llm_providers::FactorySchemaLookup>,
    ) -> Self {
        self.llm_factory_schema = Some(schema);
        self
    }

    /// Install the OAuth verifier store the
    /// `oauth_start` / `oauth_finish` handlers use to suspend
    /// PKCE state across the two RPC calls. Production wires
    /// `nexo_llm_auth::InMemoryVerifierStore::new(100)`.
    pub fn with_oauth_verifier_store(
        mut self,
        store: Arc<dyn nexo_llm_auth::VerifierStore>,
    ) -> Self {
        self.oauth_verifier_store = Some(store);
        self
    }

    /// Install the agent_events domain. Production
    /// passes a `TranscriptReader` adapter wrapping
    /// `TranscriptWriter` + `TranscriptsIndex`.
    pub fn with_agent_events_domain(mut self, reader: Arc<dyn TranscriptReader>) -> Self {
        self.transcript_reader = Some(reader);
        self
    }

    /// Install the microapp_audit
    /// domain. Production passes the same
    /// `Arc<SqliteAdminAuditWriter>` already wired as the
    /// audit writer (the type implements both reader + writer
    /// traits, sharing one connection pool).
    pub fn with_audit_reader(mut self, reader: Arc<dyn AdminAuditReader>) -> Self {
        self.audit_reader = Some(reader);
        self
    }

    /// Install the processing domain. Production
    /// passes a `ProcessingControlStore` adapter (in-memory
    /// DashMap variant in v0). `None` keeps the four
    /// `nexo/admin/processing/*` methods disabled.
    pub fn with_processing_domain(mut self, store: Arc<dyn ProcessingControlStore>) -> Self {
        self.processing_store = Some(store);
        self
    }

    /// Install the escalations domain.
    /// Production wires the in-memory adapter; SQLite-backed
    /// durable variant is a follow-up. `None` disables
    /// `nexo/admin/escalations/*`.
    pub fn with_escalations_domain(mut self, store: Arc<dyn EscalationStore>) -> Self {
        self.escalation_store = Some(store);
        self
    }

    /// Install the skills domain. Production passes
    /// an `FsSkillsStore` adapter pointed at the same skills root
    /// the `SkillLoader` reads from. `None` disables
    /// `nexo/admin/skills/*`.
    pub fn with_skills_domain(mut self, store: Arc<dyn SkillsStore>) -> Self {
        self.skills_store = Some(store);
        self
    }

    /// Install the tenants domain. Production
    /// passes a `TenantsYamlPatcher` adapter wrapping
    /// `config/tenants.yaml`. `None` disables
    /// `nexo/admin/tenants/*` (single-tenant deployments where
    /// the operator has no tenant management surface).
    pub fn with_tenants_domain(mut self, store: Arc<dyn TenantStore>) -> Self {
        self.tenant_store = Some(store);
        self
    }

    /// Install the MCP servers domain.
    /// Production passes [`super::domains::mcp::McpYamlStore`]
    /// pointed at `<config_dir>/mcp.yaml`. `None` disables
    /// `nexo/admin/mcp/*` (operator manages mcp.yaml via CLI /
    /// direct edit).
    pub fn with_mcp_domain(mut self, store: Arc<dyn McpServerStore>) -> Self {
        self.mcp_store = Some(store);
        self
    }

    /// Install the plugin restart
    /// adapter. Production wires
    /// `nexo_setup::admin_adapters::LivePluginRestarter`. `None`
    /// disables `nexo/admin/plugins/restart` (operator falls back
    /// to daemon-wide restart).
    pub fn with_plugin_restarter(mut self, restarter: Arc<dyn PluginRestarter>) -> Self {
        self.plugin_restarter = Some(restarter);
        self
    }

    /// Phase 97.1 — install the live plugin install adapter so
    /// `nexo/admin/plugins/{scan,install,uninstall}` route to the
    /// daemon's discovery walker + cargo / release download
    /// pipeline. Builder-style mirror of [`Self::with_plugin_restarter`].
    pub fn with_plugin_installer(mut self, installer: Arc<dyn PluginInstaller>) -> Self {
        self.plugin_installer = Some(installer);
        self
    }

    /// Install the plugin discovery reader (Phase 98.10).
    /// Production wires
    /// `nexo_setup::discovery_adapter::DefaultDiscoveryAdapter`
    /// which delegates to
    /// `nexo_plugin_discovery::client::DefaultDiscoveryClient`.
    pub fn with_plugin_discovery(mut self, reader: Arc<dyn DiscoveryReader>) -> Self {
        self.plugin_discovery = Some(reader);
        self
    }

    /// Install the plugin doctor reader.
    /// Production wires the live `wire_plugin_registry` +
    /// `doctor_render::render_json` pipeline. `None` disables
    /// `nexo/admin/plugins/doctor` (operator falls back to the
    /// `agent doctor plugins` CLI).
    pub fn with_plugin_doctor(mut self, reader: Arc<dyn PluginDoctorReader>) -> Self {
        self.plugin_doctor = Some(reader);
        self
    }

    /// Install the pairing-channels descriptor reader.
    /// Production wires `nexo_setup::admin_adapters::
    /// PairingChannelsReaderImpl`, which enumerates loaded plugin
    /// manifests via `wire_plugin_registry` + joins linked
    /// instances from `credentials/list`. `None` disables
    /// `nexo/admin/pairing/channels`.
    pub fn with_pairing_channels(mut self, reader: Arc<dyn PairingChannelsReader>) -> Self {
        self.pairing_channels_reader = Some(reader);
        self
    }

    /// Install the Phase 81.31 persona-snapshot reader. Without
    /// one, `agents/get` returns `persona_locales: None` and the
    /// admin wizard falls back to the single-locale flow.
    pub fn with_persona_snapshot_reader(mut self, reader: Arc<dyn PersonaSnapshotReader>) -> Self {
        self.persona_snapshot_reader = Some(reader);
        self
    }

    /// Install the Phase 81.31 persona store (write-side).
    /// Without one, `nexo/admin/persona/save_localized` returns
    /// `persona domain not configured` -32603.
    pub fn with_persona_store(mut self, store: Arc<dyn PersonaStore>) -> Self {
        self.persona_store = Some(store);
        self
    }

    /// Install the persona TEMPLATE catalog. Powers
    /// `nexo/admin/personas/list` + `…/template_get`. Without it
    /// the wizard sees an empty template list. Personas listed
    /// here are install_root-backed; the daemon never auto-merges
    /// them into `cfg.agents`.
    pub fn with_persona_catalog(
        mut self,
        catalog: Arc<dyn super::domains::persona::PersonaCatalogReader>,
    ) -> Self {
        self.persona_catalog = Some(catalog);
        self
    }

    /// Install the long-term memory query
    /// reader. Production wires
    /// `nexo_setup::admin_adapters::LiveMemoryReader` around the
    /// daemon's `LongTermMemory` instance. `None` disables
    /// `nexo/admin/memory/query` (operator falls back to the
    /// `memory.recall` agent SDK call).
    pub fn with_memory_reader(mut self, reader: Arc<dyn MemoryReader>) -> Self {
        self.memory_reader = Some(reader);
        self
    }

    /// Install the snapshot list
    /// reader. Production wires
    /// `nexo_setup::admin_adapters::LiveMemorySnapshotReader`
    /// around the daemon's `MemorySnapshotter`. `None` disables
    /// `nexo/admin/memory/list_snapshots` (operator falls back
    /// to `agent memory snapshot list` CLI).
    pub fn with_memory_snapshot_reader(mut self, reader: Arc<dyn MemorySnapshotReader>) -> Self {
        self.memory_snapshot_reader = Some(reader);
        self
    }

    /// Install the channel-outbound dispatcher
    /// used by `processing/intervention` when the action is
    /// `Reply`. Without one wired the handler returns
    /// `-32004 channel_unavailable`. Production passes a
    /// multi-channel router adapter living in `nexo-setup`.
    pub fn with_channel_outbound(mut self, outbound: Arc<dyn ChannelOutboundDispatcher>) -> Self {
        self.channel_outbound = Some(outbound);
        self
    }

    /// Install the transcript appender used by
    /// `processing/intervention` to stamp operator replies onto
    /// the agent transcript. Without one wired, replies still go
    /// out through the channel but the transcript is not
    /// modified — `ProcessingAck.transcript_stamped` reports
    /// `Some(false)` so the operator UI can surface a hint.
    /// Production wires
    /// `nexo_setup::admin_adapters::TranscriptWriterAppender`.
    pub fn with_transcript_appender(mut self, appender: Arc<dyn TranscriptAppender>) -> Self {
        self.transcript_appender = Some(appender);
        self
    }

    /// Install the channels domain. Reuses the
    /// agents-domain `YamlPatcher` + `ReloadSignal` (channels
    /// live in `agents.yaml.<id>.channels.approved`).
    pub fn with_channels_domain(self) -> Self {
        // No-op — channels already use the agents `YamlPatcher`.
        // Method kept for API symmetry with the other
        // `with_*_domain` builders + future migration to a
        // separate channels-only abstraction.
        self
    }

    /// Capability required for each method. Method routing also
    /// happens here — `None` = unknown method.
    fn required_capability(method: &str) -> Option<&'static str> {
        match method {
            "nexo/admin/echo" => Some("_echo"),
            "nexo/admin/agents/list"
            | "nexo/admin/agents/get"
            | "nexo/admin/agents/upsert"
            | "nexo/admin/agents/delete" => Some("agents_crud"),
            "nexo/admin/credentials/list"
            | "nexo/admin/credentials/register"
            | "nexo/admin/credentials/revoke" => Some("credentials_crud"),
            "nexo/admin/pairing/start"
            | "nexo/admin/pairing/status"
            | "nexo/admin/pairing/cancel" => Some("pairing_initiate"),
            "nexo/admin/llm_providers/list"
            | "nexo/admin/llm_providers/upsert"
            | "nexo/admin/llm_providers/delete"
            | "nexo/admin/llm_providers/probe"
            | "nexo/admin/llm_providers/probe_draft"
            | "nexo/admin/llm_providers/oauth_start"
            | "nexo/admin/llm_providers/oauth_finish"
            | "nexo/admin/llm_providers/catalog" => Some("llm_keys_crud"),
            // Runtime LLM completion. Distinct
            // capability from llm_keys_crud: an extension can
            // *use* the LLM (drafts, classification) without the
            // ability to mutate provider configs.
            "nexo/admin/llm/complete" => Some("llm_complete"),
            "nexo/admin/channels/list"
            | "nexo/admin/channels/approve"
            | "nexo/admin/channels/revoke"
            | "nexo/admin/channels/doctor" => Some("channels_crud"),
            // Agent events backfill domain. The
            // live notification stream uses the
            // `transcripts_subscribe` / `agent_events_subscribe_all`
            // capabilities checked at boot wire-up; the RPC
            // surface only needs `transcripts_read`.
            "nexo/admin/agent_events/list"
            | "nexo/admin/agent_events/read"
            | "nexo/admin/agent_events/search" => Some("transcripts_read"),
            // The microapp_admin_audit table
            // tail. Distinct capability from `transcripts_read`
            // because audit logs are operator-tier ops history
            // (when did each admin call happen, by whom), not
            // user-content transcripts.
            "nexo/admin/microapp_audit/tail" => Some("audit_read"),
            // Processing pause + intervention.
            // Single combined gate; per-scope sub-gates are a
            // follow-up.
            "nexo/admin/processing/pause"
            | "nexo/admin/processing/resume"
            | "nexo/admin/processing/intervention"
            | "nexo/admin/processing/state" => Some("operator_intervention"),
            // Escalations: `list` is read-only,
            // `resolve` mutates. Two granular caps so
            // operator-readonly UIs (dashboards) hold the
            // weaker grant.
            "nexo/admin/escalations/list" => Some("escalations_read"),
            "nexo/admin/escalations/resolve" => Some("escalations_resolve"),
            // Skills CRUD. Single combined gate;
            // microapps that hold this can list/get/upsert/delete.
            "nexo/admin/skills/list"
            | "nexo/admin/skills/get"
            | "nexo/admin/skills/upsert"
            | "nexo/admin/skills/delete" => Some("skills_crud"),
            // Tenants CRUD. Same combined-gate
            // pattern as skills: any microapp that holds
            // `tenants_crud` can list/get/upsert/delete the
            // operator's tenants registry.
            "nexo/admin/tenants/list"
            | "nexo/admin/tenants/get"
            | "nexo/admin/tenants/upsert"
            | "nexo/admin/tenants/delete" => Some("tenants_crud"),
            // admin/mcp/* CRUD over
            // `<config_dir>/mcp.yaml.mcp.servers`. Gated on
            // `mcp_crud`; nexo-plugin-admin declares it as
            // required so the daemon refuses to spawn the
            // plugin until the operator grants it.
            "nexo/admin/mcp/list"
            | "nexo/admin/mcp/get"
            | "nexo/admin/mcp/upsert"
            | "nexo/admin/mcp/delete" => Some("mcp_crud"),
            // admin/plugins/doctor — live
            // snapshot of plugin discovery + spawn status. Gated
            // on `plugin_doctor`; nexo-plugin-admin declares it
            // required.
            "nexo/admin/plugins/doctor" => Some("plugin_doctor"),
            // Plugin-driven pairing UI descriptor —
            // reuses `pairing_initiate` (same gate as `pairing/start`)
            // because operators able to start a pairing flow already
            // see the channel catalog implicitly.
            "nexo/admin/pairing/channels" => Some("pairing_initiate"),
            // Phase 81.31 — persona localised write
            // is operator-edit territory; reuses `agents_crud`.
            "nexo/admin/persona/save_localized" => Some("agents_crud"),
            // Persona TEMPLATE catalog (read-only). Same `agents_crud`
            // gate as the agent CRUD it feeds — the wizard needs both
            // to render a "create from template" flow end-to-end.
            "nexo/admin/personas/list" | "nexo/admin/personas/template_get" => Some("agents_crud"),
            // Manual plugin restart
            // is write+destructive; distinct capability from the
            // read-only `plugin_doctor` so security review can
            // grant them independently.
            "nexo/admin/plugins/restart" => Some("plugin_restart"),
            // Phase 97.1 — runtime plugin install/scan/uninstall.
            // All three share `plugin_install` capability so security
            // review can grant the whole lifecycle as one unit; the
            // operator can scan-only later by gating on something
            // tighter if the threat model expands.
            "nexo/admin/plugins/scan"
            | "nexo/admin/plugins/install"
            | "nexo/admin/plugins/uninstall"
            | "nexo/admin/plugins/set_enabled" => Some("plugin_install"),
            // Phase 98.10 — read-mostly discovery surface. Reuses
            // `plugin_install` capability since both verbs let an
            // operator influence which plugins exist on the host;
            // the discovery layer just surfaces the catalogue
            // *before* the install action. Keeping them on the same
            // gate avoids a second tier of cap-toggles for what is
            // effectively the same operator role.
            "nexo/admin/plugins/search"
            | "nexo/admin/plugins/compat_check"
            | "nexo/admin/plugins/refresh_index" => Some("plugin_install"),
            // Phase 99.5 — plugin admin-UI contribution surface.
            // `list`/`describe` are read-mostly; `config_set` mutates
            // a plugin's config + secrets. All three share
            // `plugin_admin_ui` so the operator grants the admin-UI
            // role as one unit (mirrors the plugin_install pattern).
            "nexo/admin/plugin_ui/list"
            | "nexo/admin/plugin_ui/describe"
            | "nexo/admin/plugin_ui/config_set" => Some("plugin_admin_ui"),
            // Long-term memory query.
            // Capability `memory_query` so an operator can grant
            // read-only memory inspection independently of broader
            // capabilities.
            "nexo/admin/memory/query" => Some("memory_query"),
            // List/delete snapshots and create/restore.
            // All four verbs share the `memory_snapshot` capability:
            // operators that already grant list+delete don't need
            // a fresh grant for create+restore — both are gated by
            // the same operator-side trust boundary.
            "nexo/admin/memory/list_snapshots"
            | "nexo/admin/memory/delete_snapshot"
            | "nexo/admin/memory/create_snapshot"
            | "nexo/admin/memory/restore_snapshot" => Some("memory_snapshot"),
            // secrets/write persists operator-
            // supplied secrets to `<state_root>/secrets/<NAME>.txt`
            // and `std::env::set_var` so existing LLM clients
            // pick up the value without a daemon restart.
            // Critical capability; INVENTORY-gated.
            "nexo/admin/secrets/write" => Some("secrets_write"),
            // Operator bearer rotation. Critical
            // capability; operator who holds it can lock out the
            // microapp from its own daemon. Granted only to UIs
            // that expose a deliberate "rotate token" action.
            "nexo/admin/auth/rotate_token" => Some("auth_rotate"),
            // WhatsApp bot bubble — list assigned bots + send a
            // manual message. Reuses the channel CRUD gate since
            // operators who can manage channels are the same set
            // that drives the bubble UI.
            "nexo/admin/whatsapp/bot/list" | "nexo/admin/whatsapp/bot/send" => {
                Some("channels_crud")
            }
            // `reload` requires any granted CRUD capability — operators
            // who can mutate yaml can also force-trigger the reload.
            // Resolution falls through to `agents_crud` since it's the
            // most likely granted capability for any UI-bearing
            // microapp.
            "nexo/admin/reload" => Some("agents_crud"),
            _ => None,
        }
    }

    /// Dispatch one admin RPC request.
    pub async fn dispatch(&self, microapp_id: &str, method: &str, params: Value) -> AdminRpcResult {
        let started = Instant::now();
        let started_at_ms = now_epoch_ms();
        // Redact sensitive fields per-method
        // BEFORE hashing so low-entropy values can't be
        // brute-forced from the audit DB hash.
        let args_hash = hash_params(&super::audit::redact_for_audit(method, &params));
        // Sniff tenant scope from params so the
        // audit row is filterable per tenant. Method routing /
        // capability gate / handler dispatch all see the same
        // value.
        let tenant_id = super::audit::extract_tenant_id(&params);

        // 1. Method routing — capability lookup serves double
        //    duty: identifies the method, names the gate. Phase
        //    81.33.b.real Stage 4 adds a fallback: methods unknown
        //    to the static map AND matched by the
        //    `plugin_admin_router` are treated as gated by
        //    `channels_crud` (reuse the existing capability so
        //    operators with channel-admin grants can call plugin
        //    admin methods without a fresh capability). Per-plugin
        //    capability grants are a follow-up if needed.
        let static_cap = Self::required_capability(method);
        let plugin_router_match = self
            .plugin_admin_router
            .as_ref()
            .and_then(|r| r.match_method(method).is_some().then_some(()));
        let Some(capability) = static_cap.or_else(|| plugin_router_match.map(|_| "channels_crud"))
        else {
            let row = AdminAuditRow {
                microapp_id: microapp_id.to_string(),
                method: method.to_string(),
                capability: "(unknown_method)".into(),
                args_hash,
                started_at_ms,
                result: AdminAuditResult::Error,
                duration_ms: started.elapsed().as_millis() as u64,
                tenant_id: tenant_id.clone(),
            };
            self.audit.append(row).await;
            return AdminRpcResult::err(AdminRpcError::MethodNotFound(format!(
                "no admin handler registered for `{method}`"
            )));
        };

        // 2. Capability gate — fail-closed if not granted.
        if !self.capabilities.check(microapp_id, capability) {
            let row = AdminAuditRow {
                microapp_id: microapp_id.to_string(),
                method: method.to_string(),
                capability: capability.to_string(),
                args_hash,
                started_at_ms,
                result: AdminAuditResult::Denied,
                duration_ms: started.elapsed().as_millis() as u64,
                tenant_id: tenant_id.clone(),
            };
            self.audit.append(row).await;
            return AdminRpcResult::err(AdminRpcError::CapabilityNotGranted {
                capability: capability.to_string(),
                method: method.to_string(),
                microapp_id: microapp_id.to_string(),
            });
        }

        // 3. Handler dispatch.
        let result = self.call_handler(microapp_id, method, params).await;

        // 4. Audit row.
        let audit_result = match &result {
            AdminRpcResult { error: Some(_), .. } => AdminAuditResult::Error,
            _ => AdminAuditResult::Ok,
        };
        let row = AdminAuditRow {
            microapp_id: microapp_id.to_string(),
            method: method.to_string(),
            capability: capability.to_string(),
            args_hash,
            started_at_ms,
            result: audit_result,
            duration_ms: started.elapsed().as_millis() as u64,
            tenant_id,
        };
        self.audit.append(row).await;
        result
    }

    /// Method router.
    async fn call_handler(&self, microapp_id: &str, method: &str, params: Value) -> AdminRpcResult {
        // Phase 81.33.b.real Stage 4 — generic plugin admin
        // forwarder. Methods whose prefix matches a manifest-
        // declared `[plugin.admin] method_prefix` get forwarded
        // via broker JSON-RPC; the plugin's subprocess handles
        // internal dispatch. Runs BEFORE the typed match arms so
        // a plugin that declares `nexo/admin/whatsapp/bot/` wins
        // over the legacy hardcoded `wa_bot_handle` block —
        // smooth migration once canonical plugins ship the
        // manifest section.
        if let (Some(router), Some(broker)) = (
            self.plugin_admin_router.as_ref(),
            self.plugin_admin_broker.as_ref(),
        ) {
            if let Some(info) = router.match_method(method) {
                match nexo_pairing::plugin_admin::forward_request(broker, info, method, params)
                    .await
                {
                    Ok(reply) => {
                        if reply.ok {
                            return AdminRpcResult::ok(reply.result);
                        }
                        return AdminRpcResult::err(AdminRpcError::Internal(reply.error));
                    }
                    Err(err) => {
                        return AdminRpcResult::err(AdminRpcError::Internal(format!(
                            "plugin admin forward failed: {err}"
                        )));
                    }
                }
            }
        }
        match method {
            "nexo/admin/echo" => AdminRpcResult::ok(serde_json::json!({
                "echoed": params,
                "microapp_id": microapp_id,
            })),
            // Phase 99.5 — plugin admin-UI domain.
            "nexo/admin/plugin_ui/list" => match &self.plugin_ui {
                Some(d) => d.list(),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin_ui domain not configured".into(),
                )),
            },
            "nexo/admin/plugin_ui/describe" => match &self.plugin_ui {
                Some(d) => d.describe(params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin_ui domain not configured".into(),
                )),
            },
            "nexo/admin/plugin_ui/config_set" => match &self.plugin_ui {
                Some(d) => d.config_set(params, self.event_emitter.clone()).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin_ui domain not configured".into(),
                )),
            },
            "nexo/admin/agents/list" => match &self.agents_yaml {
                Some(yaml) => super::domains::agents::list(yaml.as_ref(), params),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agents domain not configured".into(),
                )),
            },
            "nexo/admin/agents/get" => match &self.agents_yaml {
                Some(yaml) => {
                    let snapshot = self.persona_snapshot_reader.as_deref();
                    super::domains::agents::get_with_persona(yaml.as_ref(), snapshot, params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agents domain not configured".into(),
                )),
            },
            "nexo/admin/agents/upsert" => match (&self.agents_yaml, &self.reload_signal) {
                (Some(yaml), Some(reload)) => {
                    let trigger = reload.clone();
                    super::domains::agents::upsert(yaml.as_ref(), params, &move || trigger())
                }
                _ => AdminRpcResult::err(AdminRpcError::Internal(
                    "agents domain not configured".into(),
                )),
            },
            "nexo/admin/agents/delete" => match (&self.agents_yaml, &self.reload_signal) {
                (Some(yaml), Some(reload)) => {
                    let trigger = reload.clone();
                    super::domains::agents::delete(yaml.as_ref(), params, &move || trigger())
                }
                _ => AdminRpcResult::err(AdminRpcError::Internal(
                    "agents domain not configured".into(),
                )),
            },
            "nexo/admin/credentials/list" => match (&self.credential_store, &self.agents_yaml) {
                (Some(store), Some(yaml)) => {
                    super::domains::credentials::list(store.as_ref(), yaml.as_ref(), params)
                }
                _ => AdminRpcResult::err(AdminRpcError::Internal(
                    "credentials domain not configured".into(),
                )),
            },
            "nexo/admin/credentials/register" => {
                match (
                    &self.credential_store,
                    &self.agents_yaml,
                    &self.reload_signal,
                ) {
                    (Some(store), Some(yaml), Some(reload)) => {
                        let trigger = reload.clone();
                        // Peek at the channel
                        // field to look up the registered
                        // persister BEFORE handing the params off
                        // to the handler. Bad shapes still get
                        // `InvalidParams` from the handler — we
                        // just default to no-persister on parse
                        // failure so the existing error path
                        // wins.
                        let channel = params
                            .get("channel")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let persister =
                            channel.as_deref().and_then(|c| self.persister_for(c));
                        let outcome = super::domains::credentials::register(
                            store.as_ref(),
                            yaml.as_ref(),
                            persister,
                            params,
                            &move || trigger(),
                        )
                        .await;
                        // Phase 97.x — after a successful credential
                        // register, push a fresh `plugin.configure` to
                        // the channel's LIVE subprocess so it
                        // eager-starts polling/connecting for the
                        // just-configured instance WITHOUT a daemon
                        // restart. A subprocess channel plugin only
                        // begins `getUpdates` (telegram) etc. inside
                        // its `on_configure` handler — so re-delivering
                        // the config slice (not a restart, which would
                        // just kill the idle child) is what brings it
                        // online. Generic across telegram / email / any
                        // future channel (whatsapp also configures via
                        // its pairing `on_pair_success` path).
                        // Idempotent; missing restarter skips; a failure
                        // is logged but does NOT fail the register —
                        // the yaml write already succeeded.
                        if outcome.error.is_none() {
                            if let (Some(restarter), Some(channel)) =
                                (&self.plugin_restarter, channel.as_deref())
                            {
                                match restarter.reconfigure(channel).await {
                                    Ok(()) => tracing::info!(
                                        channel,
                                        "post-register channel reconfigure pushed"
                                    ),
                                    Err(e) => tracing::warn!(
                                        channel,
                                        error = %e,
                                        "post-register channel reconfigure failed; \
                                         configured instance may need a manual restart"
                                    ),
                                }
                            }
                        }
                        outcome
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "credentials domain not configured".into(),
                    )),
                }
            }
            "nexo/admin/credentials/revoke" => {
                match (
                    &self.credential_store,
                    &self.agents_yaml,
                    &self.reload_signal,
                ) {
                    (Some(store), Some(yaml), Some(reload)) => {
                        let trigger = reload.clone();
                        let persister = params
                            .get("channel")
                            .and_then(Value::as_str)
                            .and_then(|c| self.persister_for(c));
                        super::domains::credentials::revoke(
                            store.as_ref(),
                            yaml.as_ref(),
                            persister,
                            params,
                            &move || trigger(),
                        )
                        .await
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "credentials domain not configured".into(),
                    )),
                }
            }
            "nexo/admin/pairing/start" => match &self.pairing_store {
                Some(store) => {
                    super::domains::pairing::start_with_trigger(
                        store.clone(),
                        self.pairing_notifier.clone(),
                        &self.pairing_triggers,
                        self.pairing_handles.clone(),
                        &self.pairing_cancel_root,
                        params,
                        Some(&self.persisters),
                        self.reload_signal.as_ref(),
                    )
                    .await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "pairing domain not configured".into(),
                )),
            },
            "nexo/admin/pairing/status" => match &self.pairing_store {
                Some(store) => super::domains::pairing::status(store.as_ref(), params),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "pairing domain not configured".into(),
                )),
            },
            "nexo/admin/whatsapp/bot/list" => match &self.wa_bot_handle {
                Some(handle) => {
                    let p: nexo_tool_meta::admin::wa_bot::BotListParams =
                        match serde_json::from_value(params) {
                            Ok(v) => v,
                            Err(e) => {
                                return AdminRpcResult::err(AdminRpcError::InvalidParams(
                                    e.to_string(),
                                ));
                            }
                        };
                    match handle.list_bots(&p.agent_id).await {
                        Ok(bots) => {
                            let resp = nexo_tool_meta::admin::wa_bot::BotListResponse {
                                agent_id: p.agent_id,
                                bots: bots.into_iter().collect(),
                            };
                            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
                        }
                        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
                            "wa_bot list: {e}"
                        ))),
                    }
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "whatsapp bot domain not configured".into(),
                )),
            },
            "nexo/admin/whatsapp/bot/send" => match &self.wa_bot_handle {
                Some(handle) => {
                    let p: nexo_tool_meta::admin::wa_bot::BotSendInput =
                        match serde_json::from_value(params) {
                            Ok(v) => v,
                            Err(e) => {
                                return AdminRpcResult::err(AdminRpcError::InvalidParams(
                                    e.to_string(),
                                ));
                            }
                        };
                    if !p.bot_jid.contains("@bot") {
                        return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                            "bot_jid {} must end in @bot",
                            p.bot_jid
                        )));
                    }
                    if p.text.trim().is_empty() {
                        return AdminRpcResult::err(AdminRpcError::InvalidParams(
                            "text must not be empty".into(),
                        ));
                    }
                    match handle.send_to_bot(&p.agent_id, &p.bot_jid, &p.text).await {
                        Ok(msg_id) => {
                            let resp = nexo_tool_meta::admin::wa_bot::BotSendResponse { msg_id };
                            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
                        }
                        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
                            "wa_bot send: {e}"
                        ))),
                    }
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "whatsapp bot domain not configured".into(),
                )),
            },
            "nexo/admin/pairing/cancel" => match &self.pairing_store {
                Some(store) => super::domains::pairing::cancel_with_handles(
                    store.as_ref(),
                    self.pairing_notifier.as_deref(),
                    &self.pairing_handles,
                    params,
                ),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "pairing domain not configured".into(),
                )),
            },
            "nexo/admin/llm_providers/list" => match &self.llm_yaml {
                Some(llm) => super::domains::llm_providers::list(llm.as_ref()),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "llm_providers domain not configured".into(),
                )),
            },
            "nexo/admin/llm_providers/catalog" => match &self.llm_provider_catalog {
                Some(catalog) => super::domains::llm_providers::catalog(catalog.as_slice()),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "llm_providers catalog not configured".into(),
                )),
            },
            "nexo/admin/llm_providers/upsert" => match (&self.llm_yaml, &self.reload_signal) {
                (Some(llm), Some(reload)) => {
                    let trigger = reload.clone();
                    // secrets_store passed
                    // through so api_key_secret_value can stamp
                    // the value before yaml write. None falls
                    // through to legacy api_key_env / pre-staged
                    // api_key_secret_id paths.
                    super::domains::llm_providers::upsert(
                        llm.as_ref(),
                        self.secrets_store.as_deref(),
                        self.llm_factory_schema.as_deref(),
                        params,
                        &move || trigger(),
                    )
                    .await
                }
                _ => AdminRpcResult::err(AdminRpcError::Internal(
                    "llm_providers domain not configured".into(),
                )),
            },
            "nexo/admin/llm_providers/delete" => {
                match (&self.llm_yaml, &self.agents_yaml, &self.reload_signal) {
                    (Some(llm), Some(yaml), Some(reload)) => {
                        let trigger = reload.clone();
                        super::domains::llm_providers::delete(
                            llm.as_ref(),
                            yaml.as_ref(),
                            params,
                            &move || trigger(),
                        )
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "llm_providers domain not configured".into(),
                    )),
                }
            }
            "nexo/admin/channels/list" => match &self.agents_yaml {
                Some(yaml) => super::domains::channels::list(yaml.as_ref(), params),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "channels domain not configured".into(),
                )),
            },
            "nexo/admin/channels/approve" => match (&self.agents_yaml, &self.reload_signal) {
                (Some(yaml), Some(reload)) => {
                    let trigger = reload.clone();
                    super::domains::channels::approve(yaml.as_ref(), params, &move || trigger())
                }
                _ => AdminRpcResult::err(AdminRpcError::Internal(
                    "channels domain not configured".into(),
                )),
            },
            "nexo/admin/channels/revoke" => match (&self.agents_yaml, &self.reload_signal) {
                (Some(yaml), Some(reload)) => {
                    let trigger = reload.clone();
                    super::domains::channels::revoke(yaml.as_ref(), params, &move || trigger())
                }
                _ => AdminRpcResult::err(AdminRpcError::Internal(
                    "channels domain not configured".into(),
                )),
            },
            "nexo/admin/channels/doctor" => match &self.agents_yaml {
                Some(yaml) => super::domains::channels::doctor(yaml.as_ref(), params),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "channels domain not configured".into(),
                )),
            },
            "nexo/admin/agent_events/list" => match &self.transcript_reader {
                Some(reader) => super::domains::agent_events::list(reader.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agent_events domain not configured".into(),
                )),
            },
            "nexo/admin/agent_events/read" => match &self.transcript_reader {
                Some(reader) => super::domains::agent_events::read(reader.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agent_events domain not configured".into(),
                )),
            },
            "nexo/admin/agent_events/search" => match &self.transcript_reader {
                Some(reader) => super::domains::agent_events::search(reader.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agent_events domain not configured".into(),
                )),
            },
            // Paginated audit-row tail.
            "nexo/admin/microapp_audit/tail" => match &self.audit_reader {
                Some(reader) => super::domains::microapp_audit::tail(reader.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "microapp_audit domain not configured".into(),
                )),
            },
            "nexo/admin/processing/pause" => match &self.processing_store {
                Some(store) => {
                    // Cross-cut into escalations: auto-resolve any
                    // pending escalation matching the same
                    // scope before flipping the pause state.
                    // Best-effort — failures here are logged
                    // but never block the pause itself.
                    if let Some(escalations) = &self.escalation_store {
                        if let Ok(p) = serde_json::from_value::<
                            nexo_tool_meta::admin::processing::ProcessingPauseParams,
                        >(params.clone())
                        {
                            if let Err(e) = super::domains::escalations::auto_resolve_on_pause(
                                escalations.as_ref(),
                                self.event_emitter.as_ref(),
                                &p.scope,
                            )
                            .await
                            {
                                tracing::warn!(
                                    error = %e,
                                    "auto_resolve_on_pause failed; pausing anyway",
                                );
                            }
                        }
                    }
                    super::domains::processing::pause(
                        store.as_ref(),
                        self.event_emitter.as_ref(),
                        params,
                    )
                    .await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "processing domain not configured".into(),
                )),
            },
            "nexo/admin/processing/resume" => match &self.processing_store {
                Some(store) => {
                    super::domains::processing::resume(
                        store.as_ref(),
                        self.event_emitter.as_ref(),
                        self.transcript_appender.as_deref(),
                        params,
                    )
                    .await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "processing domain not configured".into(),
                )),
            },
            "nexo/admin/processing/intervention" => match &self.processing_store {
                Some(store) => {
                    super::domains::processing::intervention(
                        store.as_ref(),
                        self.channel_outbound.as_deref(),
                        self.transcript_appender.as_deref(),
                        params,
                    )
                    .await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "processing domain not configured".into(),
                )),
            },
            "nexo/admin/processing/state" => match &self.processing_store {
                Some(store) => super::domains::processing::state(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "processing domain not configured".into(),
                )),
            },
            "nexo/admin/escalations/list" => match &self.escalation_store {
                Some(store) => {
                    let patcher = self
                        .agents_yaml
                        .as_deref()
                        .map(|y| y as &dyn super::domains::agents::YamlPatcher);
                    super::domains::escalations::list(store.as_ref(), patcher, params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "escalations domain not configured".into(),
                )),
            },
            "nexo/admin/escalations/resolve" => match &self.escalation_store {
                Some(store) => {
                    super::domains::escalations::resolve(
                        store.as_ref(),
                        self.event_emitter.as_ref(),
                        params,
                    )
                    .await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "escalations domain not configured".into(),
                )),
            },
            "nexo/admin/skills/list" => match &self.skills_store {
                Some(store) => super::domains::skills::list(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "skills domain not configured".into(),
                )),
            },
            "nexo/admin/skills/get" => match &self.skills_store {
                Some(store) => super::domains::skills::get(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "skills domain not configured".into(),
                )),
            },
            "nexo/admin/skills/upsert" => match &self.skills_store {
                Some(store) => super::domains::skills::upsert(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "skills domain not configured".into(),
                )),
            },
            "nexo/admin/skills/delete" => match &self.skills_store {
                Some(store) => super::domains::skills::delete(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "skills domain not configured".into(),
                )),
            },
            "nexo/admin/tenants/list" => match &self.tenant_store {
                Some(store) => super::domains::tenants::list(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "tenants domain not configured".into(),
                )),
            },
            "nexo/admin/tenants/get" => match &self.tenant_store {
                Some(store) => super::domains::tenants::get(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "tenants domain not configured".into(),
                )),
            },
            "nexo/admin/tenants/upsert" => match &self.tenant_store {
                Some(store) => super::domains::tenants::upsert(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "tenants domain not configured".into(),
                )),
            },
            "nexo/admin/tenants/delete" => match &self.tenant_store {
                Some(store) => super::domains::tenants::delete(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "tenants domain not configured".into(),
                )),
            },
            "nexo/admin/mcp/list" => match &self.mcp_store {
                Some(store) => super::domains::mcp::list(store.as_ref()).await,
                None => {
                    AdminRpcResult::err(AdminRpcError::Internal("mcp domain not configured".into()))
                }
            },
            "nexo/admin/mcp/get" => match &self.mcp_store {
                Some(store) => super::domains::mcp::get(store.as_ref(), params).await,
                None => {
                    AdminRpcResult::err(AdminRpcError::Internal("mcp domain not configured".into()))
                }
            },
            "nexo/admin/mcp/upsert" => match &self.mcp_store {
                Some(store) => super::domains::mcp::upsert(store.as_ref(), params).await,
                None => {
                    AdminRpcResult::err(AdminRpcError::Internal("mcp domain not configured".into()))
                }
            },
            "nexo/admin/mcp/delete" => match &self.mcp_store {
                Some(store) => super::domains::mcp::delete(store.as_ref(), params).await,
                None => {
                    AdminRpcResult::err(AdminRpcError::Internal("mcp domain not configured".into()))
                }
            },
            "nexo/admin/plugins/restart" => match &self.plugin_restarter {
                Some(r) => super::domains::plugin_restart::restart_plugin(r.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin restart domain not configured".into(),
                )),
            },
            "nexo/admin/plugins/scan" => match &self.plugin_installer {
                Some(i) => super::domains::plugin_install::scan_plugins(i.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin install domain not configured".into(),
                )),
            },
            "nexo/admin/plugins/install" => match &self.plugin_installer {
                Some(i) => {
                    let pid = params
                        .get("crate_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let r =
                        super::domains::plugin_install::install_plugin(i.as_ref(), params).await;
                    if r.error.is_none() {
                        self.emit_plugin_ui_change(&pid, PluginUiChangeKind::Installed)
                            .await;
                    }
                    r
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin install domain not configured".into(),
                )),
            },
            "nexo/admin/plugins/uninstall" => match &self.plugin_installer {
                Some(i) => {
                    let pid = params
                        .get("plugin_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let r =
                        super::domains::plugin_install::uninstall_plugin(i.as_ref(), params).await;
                    if r.error.is_none() {
                        self.emit_plugin_ui_change(&pid, PluginUiChangeKind::Uninstalled)
                            .await;
                    }
                    r
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin install domain not configured".into(),
                )),
            },
            "nexo/admin/plugins/set_enabled" => match &self.plugin_installer {
                Some(i) => {
                    let pid = params
                        .get("plugin_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let enabled = params
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let r = super::domains::plugin_install::set_enabled_plugin(i.as_ref(), params)
                        .await;
                    if r.error.is_none() {
                        let kind = if enabled {
                            PluginUiChangeKind::Enabled
                        } else {
                            PluginUiChangeKind::Disabled
                        };
                        self.emit_plugin_ui_change(&pid, kind).await;
                    }
                    r
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin install domain not configured".into(),
                )),
            },
            "nexo/admin/plugins/doctor" => match &self.plugin_doctor {
                Some(reader) => super::domains::plugin_doctor::doctor(reader.as_ref()).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugins domain not configured".into(),
                )),
            },
            "nexo/admin/plugins/search" => match &self.plugin_discovery {
                Some(reader) => {
                    super::domains::plugin_discovery::search(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin discovery domain not configured".into(),
                )),
            },
            "nexo/admin/plugins/compat_check" => match &self.plugin_discovery {
                Some(reader) => {
                    super::domains::plugin_discovery::compat_check(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin discovery domain not configured".into(),
                )),
            },
            "nexo/admin/plugins/refresh_index" => match &self.plugin_discovery {
                Some(reader) => {
                    super::domains::plugin_discovery::refresh_index(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "plugin discovery domain not configured".into(),
                )),
            },
            "nexo/admin/persona/save_localized" => match &self.persona_store {
                Some(store) => {
                    super::domains::persona::save_localized(store.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "persona domain not configured".into(),
                )),
            },
            "nexo/admin/personas/list" => match &self.persona_catalog {
                Some(catalog) => super::domains::persona::list_personas(catalog.as_ref()).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "persona catalog domain not configured".into(),
                )),
            },
            "nexo/admin/personas/template_get" => match &self.persona_catalog {
                Some(catalog) => {
                    super::domains::persona::get_persona_template(catalog.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "persona catalog domain not configured".into(),
                )),
            },
            "nexo/admin/pairing/channels" => match &self.pairing_channels_reader {
                Some(reader) => {
                    // Best-effort locale extraction: empty params /
                    // missing field falls back to `"en"`. We do not
                    // hard-fail on bad shape so the admin gets the
                    // catalog even when it forgets to pass locale.
                    let locale = params
                        .get("locale")
                        .and_then(|v| v.as_str())
                        .unwrap_or("en")
                        .to_string();
                    super::domains::pairing_channels::list_channels(reader.as_ref(), &locale).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "pairing channels domain not configured".into(),
                )),
            },
            "nexo/admin/memory/query" => match &self.memory_reader {
                Some(reader) => super::domains::memory::query(reader.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "memory domain not configured".into(),
                )),
            },
            "nexo/admin/memory/list_snapshots" => match &self.memory_snapshot_reader {
                Some(reader) => {
                    super::domains::memory::list_snapshots(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "memory snapshot domain not configured".into(),
                )),
            },
            "nexo/admin/memory/delete_snapshot" => match &self.memory_snapshot_reader {
                Some(reader) => {
                    super::domains::memory::delete_snapshot(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "memory snapshot domain not configured".into(),
                )),
            },
            "nexo/admin/memory/create_snapshot" => match &self.memory_snapshot_reader {
                Some(reader) => {
                    super::domains::memory::create_snapshot(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "memory snapshot domain not configured".into(),
                )),
            },
            "nexo/admin/memory/restore_snapshot" => match &self.memory_snapshot_reader {
                Some(reader) => {
                    super::domains::memory::restore_snapshot(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "memory snapshot domain not configured".into(),
                )),
            },
            "nexo/admin/secrets/write" => match &self.secrets_store {
                Some(store) => super::domains::secrets::write(store.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "secrets domain not configured".into(),
                )),
            },
            "nexo/admin/llm_providers/probe" => match &self.llm_provider_probe {
                Some(p) => super::domains::llm_providers::probe(p.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "llm_providers probe not configured".into(),
                )),
            },
            "nexo/admin/llm/complete" => match &self.llm_completer {
                Some(c) => super::domains::llm::complete(c.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "llm completer not configured".into(),
                )),
            },
            "nexo/admin/llm_providers/probe_draft" => match &self.llm_provider_probe {
                Some(p) => super::domains::llm_providers::probe_draft(p.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "llm_providers probe not configured".into(),
                )),
            },
            "nexo/admin/llm_providers/oauth_start" => match &self.oauth_verifier_store {
                Some(store) => {
                    super::domains::llm_providers::oauth_start(store.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "OAuth verifier store not configured".into(),
                )),
            },
            "nexo/admin/llm_providers/oauth_finish" => {
                match (
                    &self.oauth_verifier_store,
                    &self.secrets_store,
                    &self.llm_yaml,
                    &self.reload_signal,
                ) {
                    (Some(store), Some(secrets), Some(llm), Some(reload)) => {
                        let trigger = reload.clone();
                        super::domains::llm_providers::oauth_finish(
                            store.as_ref(),
                            secrets.as_ref(),
                            llm.as_ref(),
                            params,
                            &move || trigger(),
                        )
                        .await
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "OAuth finish requires verifier_store + secrets + llm_yaml + reload".into(),
                    )),
                }
            }
            "nexo/admin/auth/rotate_token" => match &self.auth_rotator {
                Some(r) => super::domains::auth::rotate_token(r.as_ref(), params).await,
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "auth rotator not configured".into(),
                )),
            },
            "nexo/admin/reload" => match &self.reload_signal {
                Some(reload) => {
                    reload();
                    AdminRpcResult::ok(serde_json::json!({
                        "reloaded_at_ms": now_epoch_ms(),
                    }))
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "reload signal not configured".into(),
                )),
            },
            // unreachable — `required_capability` already filtered
            // unknown methods before we got here. Defensive.
            other => AdminRpcResult::err(AdminRpcError::MethodNotFound(format!(
                "no admin handler registered for `{other}`"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn dispatcher_granting(microapp_id: &str, caps: &[&str]) -> AdminRpcDispatcher {
        let mut grants = HashMap::new();
        grants.insert(
            microapp_id.to_string(),
            caps.iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
        );
        AdminRpcDispatcher::new().with_capabilities(CapabilitySet::from_grants(grants))
    }

    #[derive(Debug, Default)]
    struct RecordingEmitter {
        events: std::sync::Mutex<Vec<AgentEventKind>>,
    }
    #[async_trait::async_trait]
    impl crate::agent::agent_events::AgentEventEmitter for RecordingEmitter {
        async fn emit(&self, event: AgentEventKind) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn emit_plugin_ui_change_fires_with_kind_and_id() {
        let em = Arc::new(RecordingEmitter::default());
        let d = AdminRpcDispatcher::new().with_event_emitter(em.clone());
        d.emit_plugin_ui_change("telegram", PluginUiChangeKind::Installed)
            .await;
        let events = em.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEventKind::PluginUiChanged {
                event_kind,
                plugin_id,
                ..
            } => {
                assert_eq!(*event_kind, PluginUiChangeKind::Installed);
                assert_eq!(plugin_id, "telegram");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn emit_plugin_ui_change_noop_on_empty_id_or_no_emitter() {
        let em = Arc::new(RecordingEmitter::default());
        let d = AdminRpcDispatcher::new().with_event_emitter(em.clone());
        d.emit_plugin_ui_change("", PluginUiChangeKind::Disabled)
            .await;
        assert!(em.events.lock().unwrap().is_empty());
        // No emitter wired → no panic, no-op.
        AdminRpcDispatcher::new()
            .emit_plugin_ui_change("telegram", PluginUiChangeKind::Enabled)
            .await;
    }

    #[tokio::test]
    async fn dispatch_echo_returns_params_when_echo_capability_granted() {
        let d = dispatcher_granting("agent-creator", &["_echo"]);
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/echo",
                serde_json::json!({ "x": 1, "y": "hello" }),
            )
            .await;
        let value = result.result.expect("ok");
        assert_eq!(value["echoed"]["x"], 1);
        assert_eq!(value["echoed"]["y"], "hello");
        assert_eq!(value["microapp_id"], "agent-creator");
    }

    #[tokio::test]
    async fn dispatch_echo_denies_when_capability_not_granted() {
        let d = AdminRpcDispatcher::new();
        let result = d
            .dispatch("agent-creator", "nexo/admin/echo", Value::Null)
            .await;
        let err = result.error.expect("error");
        match err {
            AdminRpcError::CapabilityNotGranted {
                capability,
                method,
                microapp_id,
            } => {
                assert_eq!(capability, "_echo");
                assert_eq!(method, "nexo/admin/echo");
                assert_eq!(microapp_id, "agent-creator");
            }
            other => panic!("expected CapabilityNotGranted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_unknown_method_returns_method_not_found() {
        let d = AdminRpcDispatcher::new();
        let result = d
            .dispatch("agent-creator", "nexo/admin/totally_unknown", Value::Null)
            .await;
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::MethodNotFound(_)));
        assert_eq!(err.code(), -32601);
    }

    #[tokio::test]
    async fn dispatch_tenants_list_returns_internal_when_store_unwired() {
        // Capability gate + handler
        // routing both exist. Without `with_tenants_domain`, the
        // handler arm returns a typed `tenants domain not
        // configured` Internal error so microapps surface a
        // clear wire-up gap.
        let d = dispatcher_granting("agent-creator", &["tenants_crud"]);
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/tenants/list",
                serde_json::json!({}),
            )
            .await;
        let err = result.error.expect("error");
        match err {
            AdminRpcError::Internal(msg) => {
                assert!(
                    msg.contains("tenants domain not configured"),
                    "expected typed gap error; got {msg}"
                );
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_tenants_list_denies_when_capability_not_granted() {
        // No `tenants_crud` grant → capability gate rejects
        // before the handler arm runs.
        let d = AdminRpcDispatcher::new();
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/tenants/list",
                serde_json::json!({}),
            )
            .await;
        let err = result.error.expect("error");
        match err {
            AdminRpcError::CapabilityNotGranted { capability, .. } => {
                assert_eq!(capability, "tenants_crud");
            }
            other => panic!("expected CapabilityNotGranted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_tenants_list_routes_to_handler_when_store_wired() {
        use super::super::domains::tenants::TenantStore;
        use async_trait::async_trait;
        use nexo_tool_meta::admin::tenants::{
            TenantDetail, TenantSummary, TenantsListFilter, TenantsUpsertInput,
        };

        #[derive(Debug, Default)]
        struct StaticStore;
        #[async_trait]
        impl TenantStore for StaticStore {
            async fn list(
                &self,
                _filter: &TenantsListFilter,
            ) -> anyhow::Result<Vec<TenantSummary>> {
                Ok(vec![TenantSummary {
                    id: "acme".into(),
                    display_name: "Acme".into(),
                    active: true,
                    agent_count: 0,
                    created_at: chrono::Utc::now(),
                }])
            }
            async fn get(&self, _id: &str) -> anyhow::Result<Option<TenantDetail>> {
                Ok(None)
            }
            async fn upsert(
                &self,
                _input: TenantsUpsertInput,
            ) -> anyhow::Result<(TenantDetail, bool)> {
                anyhow::bail!("not used")
            }
            async fn delete(&self, _id: &str, _purge: bool) -> anyhow::Result<(bool, Vec<String>)> {
                Ok((false, vec![]))
            }
        }

        let d = dispatcher_granting("agent-creator", &["tenants_crud"])
            .with_tenants_domain(Arc::new(StaticStore));
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/tenants/list",
                serde_json::json!({}),
            )
            .await;
        let value = result.result.expect("ok");
        assert_eq!(value["tenants"][0]["id"], "acme");
    }

    #[tokio::test]
    async fn audit_writer_records_each_call_with_args_hash() {
        let writer = Arc::new(InMemoryAuditWriter::new());
        let d = dispatcher_granting("agent-creator", &["_echo"]).with_audit_writer(writer.clone());
        let _ = d
            .dispatch(
                "agent-creator",
                "nexo/admin/echo",
                serde_json::json!({ "x": 1 }),
            )
            .await;
        let row = writer.last().expect("row recorded");
        assert_eq!(row.microapp_id, "agent-creator");
        assert_eq!(row.method, "nexo/admin/echo");
        assert_eq!(row.capability, "_echo");
        assert_eq!(row.result, AdminAuditResult::Ok);
        assert_eq!(row.args_hash.len(), 64); // sha256 hex
    }

    #[tokio::test]
    async fn audit_writer_records_denial_with_capability_field() {
        let writer = Arc::new(InMemoryAuditWriter::new());
        // No capability granted — denial path.
        let d = AdminRpcDispatcher::new().with_audit_writer(writer.clone());
        let _ = d
            .dispatch("agent-creator", "nexo/admin/echo", Value::Null)
            .await;
        let row = writer.last().expect("row recorded");
        assert_eq!(row.result, AdminAuditResult::Denied);
        assert_eq!(row.capability, "_echo");
    }

    #[tokio::test]
    async fn audit_writer_records_unknown_method_as_error() {
        let writer = Arc::new(InMemoryAuditWriter::new());
        let d = dispatcher_granting("agent-creator", &["_echo"]).with_audit_writer(writer.clone());
        let _ = d
            .dispatch("agent-creator", "nexo/admin/nonexistent", Value::Null)
            .await;
        let row = writer.last().expect("row recorded");
        assert_eq!(row.result, AdminAuditResult::Error);
        assert_eq!(row.capability, "(unknown_method)");
    }

    #[tokio::test]
    async fn reload_handler_invokes_signal_when_capability_granted() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let reload: ReloadSignal = Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        let d = dispatcher_granting("agent-creator", &["agents_crud"]);
        // We need to manually wire the reload_signal field since
        // `with_agents_domain` requires a YamlPatcher we don't
        // need for this test. Use the public builder approach via
        // a trivial mock yaml.
        struct NoopYaml;
        impl super::super::domains::agents::YamlPatcher for NoopYaml {
            fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
                Ok(vec![])
            }
            fn read_agent_field(&self, _: &str, _: &str) -> anyhow::Result<Option<Value>> {
                Ok(None)
            }
            fn upsert_agent_field(&self, _: &str, _: &str, _: Value) -> anyhow::Result<()> {
                Ok(())
            }
            fn remove_agent(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
        }
        let d = d.with_agents_domain(Arc::new(NoopYaml), reload);
        let result = d
            .dispatch("agent-creator", "nexo/admin/reload", Value::Null)
            .await;
        assert!(result.result.is_some());
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn reload_handler_denies_without_capability() {
        let d = AdminRpcDispatcher::new();
        let result = d
            .dispatch("agent-creator", "nexo/admin/reload", Value::Null)
            .await;
        let err = result.error.expect("error");
        match err {
            AdminRpcError::CapabilityNotGranted { capability, .. } => {
                assert_eq!(capability, "agents_crud");
            }
            other => panic!("expected CapabilityNotGranted, got {other:?}"),
        }
    }

    #[test]
    fn capability_not_granted_emits_structured_data() {
        let err = AdminRpcError::CapabilityNotGranted {
            capability: "agents_crud".into(),
            method: "nexo/admin/agents/upsert".into(),
            microapp_id: "agent-creator".into(),
        };
        assert_eq!(err.code(), -32004);
        let data = err.data().expect("structured data");
        assert_eq!(data["capability"], "agents_crud");
        assert_eq!(data["microapp_id"], "agent-creator");
        assert_eq!(data["method"], "nexo/admin/agents/upsert");
    }

    #[test]
    fn admin_rpc_error_code_table() {
        assert_eq!(AdminRpcError::MethodNotFound("x".into()).code(), -32601);
        assert_eq!(AdminRpcError::InvalidParams("x".into()).code(), -32602);
        assert_eq!(AdminRpcError::Internal("x".into()).code(), -32603);
    }

    /// `secrets/write` requires `secrets_write`
    /// capability + an installed `SecretsStore`. Test the
    /// happy-path routing.
    #[tokio::test]
    async fn dispatcher_routes_secrets_write_with_capability_and_store() {
        use async_trait::async_trait;
        use nexo_tool_meta::admin::secrets::SecretsWriteResponse;
        use std::path::PathBuf;

        struct StaticStore;
        #[async_trait]
        impl super::super::domains::secrets::SecretsStore for StaticStore {
            async fn write(
                &self,
                name: &str,
                _value: &str,
            ) -> Result<SecretsWriteResponse, AdminRpcError> {
                Ok(SecretsWriteResponse {
                    path: PathBuf::from(format!("/test/secrets/{name}.txt")),
                    overwrote_env: false,
                })
            }
        }

        let d = dispatcher_granting("agent-creator", &["secrets_write"])
            .with_secrets_domain(Arc::new(StaticStore));
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/secrets/write",
                serde_json::json!({"name": "MINIMAX_API_KEY", "value": "sk-test"}),
            )
            .await;
        let value = result.result.expect("ok");
        assert_eq!(value["path"], "/test/secrets/MINIMAX_API_KEY.txt");
        assert_eq!(value["overwrote_env"], false);
    }

    /// Without the `secrets_write` capability, dispatch is
    /// rejected at the gate before the handler runs.
    #[tokio::test]
    async fn dispatcher_secrets_write_capability_denied() {
        let d = dispatcher_granting("agent-creator", &["agents_crud"]);
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/secrets/write",
                serde_json::json!({"name": "MINIMAX_API_KEY", "value": "sk-test"}),
            )
            .await;
        let err = result.error.expect("denied");
        match err {
            AdminRpcError::CapabilityNotGranted { capability, .. } => {
                assert_eq!(capability, "secrets_write");
            }
            other => panic!("expected CapabilityNotGranted, got {other:?}"),
        }
    }

    /// With the capability granted but no SecretsStore wired,
    /// the dispatcher returns `Internal` so operators can tell
    /// it's a misconfiguration vs an unknown method.
    #[tokio::test]
    async fn dispatcher_secrets_write_internal_when_store_missing() {
        let d = dispatcher_granting("agent-creator", &["secrets_write"]);
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/secrets/write",
                serde_json::json!({"name": "MINIMAX_API_KEY", "value": "sk-test"}),
            )
            .await;
        let err = result.error.expect("internal");
        match err {
            AdminRpcError::Internal(msg) => {
                assert!(msg.contains("secrets domain not configured"));
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// `llm_providers/probe` requires
    /// `llm_keys_crud` capability + an installed
    /// `LlmProvidersProbe`. Test the happy-path routing.
    #[tokio::test]
    async fn dispatcher_routes_llm_providers_probe_with_capability_and_probe_wired() {
        use async_trait::async_trait;
        use nexo_tool_meta::admin::llm_providers::LlmProviderProbeResponse;

        struct StaticProbe;
        #[async_trait]
        impl super::super::domains::llm_providers::LlmProvidersProbe for StaticProbe {
            async fn probe(
                &self,
                _provider_id: &str,
                _tenant_id: Option<&str>,
            ) -> Result<LlmProviderProbeResponse, AdminRpcError> {
                Ok(LlmProviderProbeResponse {
                    ok: true,
                    status: 200,
                    latency_ms: 17,
                    model_count: Some(3),
                    model_names: None,
                    error: None,
                })
            }
        }

        let d = dispatcher_granting("agent-creator", &["llm_keys_crud"])
            .with_llm_provider_probe(Arc::new(StaticProbe));
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/llm_providers/probe",
                serde_json::json!({"provider_id": "minimax"}),
            )
            .await;
        let value = result.result.expect("ok");
        assert_eq!(value["ok"], true);
        assert_eq!(value["status"], 200);
        assert_eq!(value["model_count"], 3);
    }

    /// Without the `llm_keys_crud` capability, dispatch is
    /// rejected at the gate before the handler runs.
    #[tokio::test]
    async fn dispatcher_llm_providers_probe_capability_denied() {
        let d = dispatcher_granting("agent-creator", &["agents_crud"]);
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/llm_providers/probe",
                serde_json::json!({"provider_id": "minimax"}),
            )
            .await;
        let err = result.error.expect("denied");
        match err {
            AdminRpcError::CapabilityNotGranted { capability, .. } => {
                assert_eq!(capability, "llm_keys_crud");
            }
            other => panic!("expected CapabilityNotGranted, got {other:?}"),
        }
    }

    /// With capability granted but no probe wired, the
    /// dispatcher returns `Internal` so operators can tell
    /// it's a misconfiguration vs an unknown method.
    #[tokio::test]
    async fn dispatcher_llm_providers_probe_internal_when_unwired() {
        let d = dispatcher_granting("agent-creator", &["llm_keys_crud"]);
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/llm_providers/probe",
                serde_json::json!({"provider_id": "minimax"}),
            )
            .await;
        let err = result.error.expect("internal");
        match err {
            AdminRpcError::Internal(msg) => {
                assert!(msg.contains("llm_providers probe not configured"));
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    // ── Capability-gate denial coverage ──
    //
    // Mirrors `dispatch_tenants_list_denies_when_capability_not_granted`
    // for every admin verb shipped in the 2026-05-10 wave. Each
    // test confirms the capability name resolved by
    // `capability_for_method` and that the dispatcher refuses
    // BEFORE entering the handler arm (so a missing slot wiring
    // can never accidentally surface user input via "domain not
    // configured" Internal).

    async fn assert_capability_gate_denies(
        method: &str,
        expected_capability: &str,
        params: serde_json::Value,
    ) {
        let d = AdminRpcDispatcher::new();
        let result = d.dispatch("agent-creator", method, params).await;
        let err = result.error.expect("error");
        match err {
            AdminRpcError::CapabilityNotGranted { capability, .. } => {
                assert_eq!(
                    capability, expected_capability,
                    "method `{method}` should be gated by `{expected_capability}`"
                );
            }
            other => panic!("expected CapabilityNotGranted for {method}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_memory_query_denies_without_capability() {
        assert_capability_gate_denies(
            "nexo/admin/memory/query",
            "memory_query",
            serde_json::json!({"agent_id": "ana", "query": "x", "limit": 5}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_memory_list_snapshots_denies_without_capability() {
        assert_capability_gate_denies(
            "nexo/admin/memory/list_snapshots",
            "memory_snapshot",
            serde_json::json!({"agent_id": "ana", "tenant": "default"}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_memory_delete_snapshot_denies_without_capability() {
        assert_capability_gate_denies(
            "nexo/admin/memory/delete_snapshot",
            "memory_snapshot",
            serde_json::json!({"agent_id": "ana", "tenant": "default", "id": "x"}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_memory_create_snapshot_denies_without_capability() {
        assert_capability_gate_denies(
            "nexo/admin/memory/create_snapshot",
            "memory_snapshot",
            serde_json::json!({"agent_id": "ana", "tenant": "default", "label": null, "encrypt": false}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_memory_restore_snapshot_denies_without_capability() {
        assert_capability_gate_denies(
            "nexo/admin/memory/restore_snapshot",
            "memory_snapshot",
            serde_json::json!({
                "agent_id": "ana",
                "tenant": "default",
                "snapshot_id": "x",
                "dry_run": true,
            }),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_plugins_doctor_denies_without_capability() {
        assert_capability_gate_denies(
            "nexo/admin/plugins/doctor",
            "plugin_doctor",
            serde_json::json!({}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_plugins_restart_denies_without_capability() {
        assert_capability_gate_denies(
            "nexo/admin/plugins/restart",
            "plugin_restart",
            serde_json::json!({"plugin_id": "browser"}),
        )
        .await;
    }

    // ── Slot-not-wired contract tests ────
    //
    // Mirror `dispatch_tenants_list_returns_internal_when_store_unwired`
    // for every admin verb shipped in the 2026-05-10 wave. With
    // capability granted but the slot field still `None`, the
    // dispatcher must return a typed `Internal` error whose message
    // contains a `<domain> domain not configured` substring so
    // microapps can detect the wire-up gap reliably.

    async fn assert_slot_not_wired_internal_contains(
        capabilities: &[&str],
        method: &str,
        expected_substring: &str,
        params: serde_json::Value,
    ) {
        let d = dispatcher_granting("agent-creator", capabilities);
        let result = d.dispatch("agent-creator", method, params).await;
        let err = result.error.expect("error");
        match err {
            AdminRpcError::Internal(msg) => {
                assert!(
                    msg.contains(expected_substring),
                    "method `{method}` slot-not-wired Internal must contain `{expected_substring}`; got `{msg}`"
                );
            }
            other => panic!("method `{method}` expected Internal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_memory_query_returns_internal_when_reader_unwired() {
        assert_slot_not_wired_internal_contains(
            &["memory_query"],
            "nexo/admin/memory/query",
            "memory domain not configured",
            serde_json::json!({"agent_id": "ana", "query": "x", "limit": 5}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_memory_list_snapshots_returns_internal_when_reader_unwired() {
        assert_slot_not_wired_internal_contains(
            &["memory_snapshot"],
            "nexo/admin/memory/list_snapshots",
            "memory snapshot domain not configured",
            serde_json::json!({"agent_id": "ana", "tenant": "default"}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_memory_delete_snapshot_returns_internal_when_reader_unwired() {
        assert_slot_not_wired_internal_contains(
            &["memory_snapshot"],
            "nexo/admin/memory/delete_snapshot",
            "memory snapshot domain not configured",
            serde_json::json!({"agent_id": "ana", "tenant": "default", "id": "x"}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_memory_create_snapshot_returns_internal_when_reader_unwired() {
        assert_slot_not_wired_internal_contains(
            &["memory_snapshot"],
            "nexo/admin/memory/create_snapshot",
            "memory snapshot domain not configured",
            serde_json::json!({
                "agent_id": "ana",
                "tenant": "default",
                "label": null,
                "encrypt": false,
            }),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_memory_restore_snapshot_returns_internal_when_reader_unwired() {
        assert_slot_not_wired_internal_contains(
            &["memory_snapshot"],
            "nexo/admin/memory/restore_snapshot",
            "memory snapshot domain not configured",
            serde_json::json!({
                "agent_id": "ana",
                "tenant": "default",
                "snapshot_id": "x",
                "dry_run": true,
            }),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_plugins_doctor_returns_internal_when_reader_unwired() {
        assert_slot_not_wired_internal_contains(
            &["plugin_doctor"],
            "nexo/admin/plugins/doctor",
            "plugins domain not configured",
            serde_json::json!({}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_plugins_restart_returns_internal_when_restarter_unwired() {
        assert_slot_not_wired_internal_contains(
            &["plugin_restart"],
            "nexo/admin/plugins/restart",
            "plugin restart domain not configured",
            serde_json::json!({"plugin_id": "browser"}),
        )
        .await;
    }
}
