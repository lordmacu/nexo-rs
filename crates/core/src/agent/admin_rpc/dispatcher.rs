//! Phase 82.10.a — admin RPC dispatcher core.
//!
//! Single entry point `AdminRpcDispatcher::dispatch(microapp_id,
//! method, params) -> AdminRpcResult` invoked by the microapp
//! transport adapter when a JSON-RPC frame with `app:` ID prefix
//! arrives. Returns the typed result/error pair; caller frames +
//! writes the response.
//!
//! Sub-phase scope:
//! - **82.10.a** (now): single mock `nexo/admin/echo` handler. No
//!   capability gate (always allow), no audit log. Validates
//!   wire-shape end-to-end before adding domain logic.
//! - **82.10.b**: capability gate + audit log writer. `echo` will
//!   require `agents_crud` (any granted capability suffices for
//!   echo testing).
//! - **82.10.c-f**: register actual domain handlers
//!   (agents/credentials/pairing/llm_providers/channels).

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use thiserror::Error;

use super::audit::{
    hash_params, now_epoch_ms, AdminAuditResult, AdminAuditRow, AdminAuditWriter,
    InMemoryAuditWriter,
};
use super::capabilities::CapabilitySet;
use super::domains::agent_events::TranscriptReader;
use super::domains::agents::YamlPatcher;
use super::domains::credentials::CredentialStore;
use super::domains::escalations::EscalationStore;
use super::domains::llm_providers::LlmYamlPatcher;
use super::domains::pairing::{PairingChallengeStore, PairingNotifier};
use super::domains::processing::ProcessingControlStore;
use super::channel_outbound::ChannelOutboundDispatcher;
use super::domains::skills::SkillsStore;
use super::domains::tenants::TenantStore;

/// Reload signal callback — invoked by domain handlers after
/// successful yaml mutations to trigger Phase 18 hot-reload.
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
    /// `-32004` — operator did not grant `capability` to this
    /// microapp via `extensions.yaml.<id>.capabilities_grant`.
    /// Wired in 82.10.b.
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

/// Phase 82.10 admin RPC dispatcher.
///
/// Routes `nexo/admin/<domain>/<method>` requests to handlers,
/// consults [`CapabilitySet`] for the operator-granted capability
/// before each call, writes one [`AdminAuditRow`] per dispatch.
#[derive(Clone)]
pub struct AdminRpcDispatcher {
    capabilities: Arc<CapabilitySet>,
    audit: Arc<dyn AdminAuditWriter>,
    /// Phase 82.10.c — yaml mutation surface used by the
    /// `agents` domain handlers. `None` = domain unavailable
    /// (returns -32601 for `nexo/admin/agents/*`). Production
    /// wiring constructs the adapter from `nexo_setup::yaml_patch`.
    agents_yaml: Option<Arc<dyn YamlPatcher>>,
    /// Phase 82.10.c — Phase 18 reload trigger called after
    /// successful yaml mutations. `None` = no-op (for early-boot
    /// tests).
    reload_signal: Option<ReloadSignal>,
    /// Phase 82.10.d — credential filesystem store. `None`
    /// disables `nexo/admin/credentials/*`.
    credential_store: Option<Arc<dyn CredentialStore>>,
    /// Phase 82.10.e — pairing challenge store. `None` disables
    /// `nexo/admin/pairing/*`.
    pairing_store: Option<Arc<dyn PairingChallengeStore>>,
    /// Phase 82.10.e — push channel for
    /// `nexo/notify/pairing_status_changed`. `None` = best-effort
    /// (poll only, notifications dropped).
    pairing_notifier: Option<Arc<dyn PairingNotifier>>,
    /// Phase 82.10.f — `llm.yaml` mutator. `None` disables
    /// `nexo/admin/llm_providers/*`.
    llm_yaml: Option<Arc<dyn LlmYamlPatcher>>,
    /// Phase 82.11 — transcripts read surface. `None` disables
    /// `nexo/admin/agent_events/*`.
    transcript_reader: Option<Arc<dyn TranscriptReader>>,
    /// Phase 82.13 — processing control store. `None` disables
    /// `nexo/admin/processing/*`.
    processing_store: Option<Arc<dyn ProcessingControlStore>>,
    /// Phase 82.14 — escalation store. `None` disables
    /// `nexo/admin/escalations/*`. When BOTH this and
    /// `processing_store` are configured, a `pause` call
    /// auto-flips any matching `Pending` escalation to
    /// `Resolved { OperatorTakeover }`.
    escalation_store: Option<Arc<dyn EscalationStore>>,
    /// Phase 83.8 — skills CRUD store. `None` disables
    /// `nexo/admin/skills/*`. Production wires
    /// `nexo_setup::admin_adapters::FsSkillsStore` against the
    /// existing `SkillLoader` filesystem layout.
    skills_store: Option<Arc<dyn SkillsStore>>,
    /// Phase 83.8.4.a — channel-outbound dispatcher used by
    /// `processing/intervention` when the action is `Reply`.
    /// `None` keeps the wire surface alive (`-32601` style
    /// rejection) but operator replies fail with
    /// `channel_unavailable`. Production wires the multi-channel
    /// router adapter living in `nexo-setup`.
    channel_outbound: Option<Arc<dyn ChannelOutboundDispatcher>>,
    /// Phase 83.8.12 — multi-tenant SaaS registry. `None`
    /// disables `nexo/admin/tenants/*` (single-tenant
    /// deployments where there is no operator-level tenant
    /// management). Production wires
    /// `nexo_setup::admin_adapters::TenantsYamlPatcher`.
    tenant_store: Option<Arc<dyn TenantStore>>,
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
            pairing_store: None,
            pairing_notifier: None,
            llm_yaml: None,
            transcript_reader: None,
            processing_store: None,
            escalation_store: None,
            skills_store: None,
            channel_outbound: None,
            tenant_store: None,
        }
    }

    /// Replace the capability set. Boot wiring calls this once
    /// after [`super::validate_capabilities_at_boot`] returns OK.
    pub fn with_capabilities(mut self, capabilities: Arc<CapabilitySet>) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Replace the audit writer. Tests inject in-memory; SQLite
    /// writer lands in 82.10.g.
    pub fn with_audit_writer(mut self, writer: Arc<dyn AdminAuditWriter>) -> Self {
        self.audit = writer;
        self
    }

    /// Phase 82.10.c — install the agents domain. Production
    /// passes a `YamlPatcher` adapter wrapping
    /// `nexo_setup::yaml_patch::*`.
    pub fn with_agents_domain(
        mut self,
        yaml: Arc<dyn YamlPatcher>,
        reload: ReloadSignal,
    ) -> Self {
        self.agents_yaml = Some(yaml);
        self.reload_signal = Some(reload);
        self
    }

    /// Phase 82.10.d — install the credentials domain. Reuses
    /// the agents-domain `YamlPatcher` + `ReloadSignal` (must be
    /// installed first via `with_agents_domain`).
    pub fn with_credentials_domain(mut self, store: Arc<dyn CredentialStore>) -> Self {
        self.credential_store = Some(store);
        self
    }

    /// Phase 82.10.e — install the pairing domain. `notifier`
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

    /// Phase 82.10.f — install the llm_providers domain.
    /// Production passes an `LlmYamlPatcher` adapter pointed at
    /// `llm.yaml`.
    pub fn with_llm_providers_domain(mut self, llm_yaml: Arc<dyn LlmYamlPatcher>) -> Self {
        self.llm_yaml = Some(llm_yaml);
        self
    }

    /// Phase 82.11 — install the agent_events domain. Production
    /// passes a `TranscriptReader` adapter wrapping
    /// `TranscriptWriter` + `TranscriptsIndex`.
    pub fn with_agent_events_domain(
        mut self,
        reader: Arc<dyn TranscriptReader>,
    ) -> Self {
        self.transcript_reader = Some(reader);
        self
    }

    /// Phase 82.13 — install the processing domain. Production
    /// passes a `ProcessingControlStore` adapter (in-memory
    /// DashMap variant in v0). `None` keeps the four
    /// `nexo/admin/processing/*` methods disabled.
    pub fn with_processing_domain(
        mut self,
        store: Arc<dyn ProcessingControlStore>,
    ) -> Self {
        self.processing_store = Some(store);
        self
    }

    /// Phase 82.14 — install the escalations domain.
    /// Production wires the in-memory adapter; SQLite-backed
    /// durable variant is a 82.14.b follow-up. `None` disables
    /// `nexo/admin/escalations/*`.
    pub fn with_escalations_domain(
        mut self,
        store: Arc<dyn EscalationStore>,
    ) -> Self {
        self.escalation_store = Some(store);
        self
    }

    /// Phase 83.8 — install the skills domain. Production passes
    /// an `FsSkillsStore` adapter pointed at the same skills root
    /// the `SkillLoader` reads from. `None` disables
    /// `nexo/admin/skills/*`.
    pub fn with_skills_domain(mut self, store: Arc<dyn SkillsStore>) -> Self {
        self.skills_store = Some(store);
        self
    }

    /// Phase 83.8.4.a — install the channel-outbound dispatcher
    /// used by `processing/intervention` when the action is
    /// `Reply`. Without one wired the handler returns
    /// `-32004 channel_unavailable`. Production passes a
    /// multi-channel router adapter living in `nexo-setup`.
    pub fn with_channel_outbound(
        mut self,
        outbound: Arc<dyn ChannelOutboundDispatcher>,
    ) -> Self {
        self.channel_outbound = Some(outbound);
        self
    }

    /// Phase 82.10.f — install the channels domain. Reuses the
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
            | "nexo/admin/llm_providers/delete" => Some("llm_keys_crud"),
            "nexo/admin/channels/list"
            | "nexo/admin/channels/approve"
            | "nexo/admin/channels/revoke"
            | "nexo/admin/channels/doctor" => Some("channels_crud"),
            // Phase 82.11 — agent events backfill domain. The
            // live notification stream uses the
            // `transcripts_subscribe` / `agent_events_subscribe_all`
            // capabilities checked at boot wire-up; the RPC
            // surface only needs `transcripts_read`.
            "nexo/admin/agent_events/list"
            | "nexo/admin/agent_events/read"
            | "nexo/admin/agent_events/search" => Some("transcripts_read"),
            // Phase 82.13 — processing pause + intervention.
            // Single combined gate; per-scope sub-gates are a
            // 82.13.b follow-up.
            "nexo/admin/processing/pause"
            | "nexo/admin/processing/resume"
            | "nexo/admin/processing/intervention"
            | "nexo/admin/processing/state" => Some("operator_intervention"),
            // Phase 82.14 — escalations: `list` is read-only,
            // `resolve` mutates. Two granular caps so
            // operator-readonly UIs (dashboards) hold the
            // weaker grant.
            "nexo/admin/escalations/list" => Some("escalations_read"),
            "nexo/admin/escalations/resolve" => Some("escalations_resolve"),
            // Phase 83.8 — skills CRUD. Single combined gate;
            // microapps that hold this can list/get/upsert/delete.
            "nexo/admin/skills/list"
            | "nexo/admin/skills/get"
            | "nexo/admin/skills/upsert"
            | "nexo/admin/skills/delete" => Some("skills_crud"),
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
    pub async fn dispatch(
        &self,
        microapp_id: &str,
        method: &str,
        params: Value,
    ) -> AdminRpcResult {
        let started = Instant::now();
        let started_at_ms = now_epoch_ms();
        let args_hash = hash_params(&params);

        // 1. Method routing — capability lookup serves double
        //    duty: identifies the method, names the gate.
        let Some(capability) = Self::required_capability(method) else {
            let row = AdminAuditRow {
                microapp_id: microapp_id.to_string(),
                method: method.to_string(),
                capability: "(unknown_method)".into(),
                args_hash,
                started_at_ms,
                result: AdminAuditResult::Error,
                duration_ms: started.elapsed().as_millis() as u64,
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
        };
        self.audit.append(row).await;
        result
    }

    /// Method router.
    async fn call_handler(
        &self,
        microapp_id: &str,
        method: &str,
        params: Value,
    ) -> AdminRpcResult {
        match method {
            "nexo/admin/echo" => AdminRpcResult::ok(serde_json::json!({
                "echoed": params,
                "microapp_id": microapp_id,
            })),
            "nexo/admin/agents/list" => match &self.agents_yaml {
                Some(yaml) => super::domains::agents::list(yaml.as_ref(), params),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agents domain not configured".into(),
                )),
            },
            "nexo/admin/agents/get" => match &self.agents_yaml {
                Some(yaml) => super::domains::agents::get(yaml.as_ref(), params),
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
            "nexo/admin/credentials/list" => {
                match (&self.credential_store, &self.agents_yaml) {
                    (Some(store), Some(yaml)) => {
                        super::domains::credentials::list(store.as_ref(), yaml.as_ref(), params)
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "credentials domain not configured".into(),
                    )),
                }
            }
            "nexo/admin/credentials/register" => {
                match (&self.credential_store, &self.agents_yaml, &self.reload_signal) {
                    (Some(store), Some(yaml), Some(reload)) => {
                        let trigger = reload.clone();
                        super::domains::credentials::register(
                            store.as_ref(),
                            yaml.as_ref(),
                            params,
                            &move || trigger(),
                        )
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "credentials domain not configured".into(),
                    )),
                }
            }
            "nexo/admin/credentials/revoke" => {
                match (&self.credential_store, &self.agents_yaml, &self.reload_signal) {
                    (Some(store), Some(yaml), Some(reload)) => {
                        let trigger = reload.clone();
                        super::domains::credentials::revoke(
                            store.as_ref(),
                            yaml.as_ref(),
                            params,
                            &move || trigger(),
                        )
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "credentials domain not configured".into(),
                    )),
                }
            }
            "nexo/admin/pairing/start" => match &self.pairing_store {
                Some(store) => super::domains::pairing::start(store.as_ref(), params),
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
            "nexo/admin/pairing/cancel" => match &self.pairing_store {
                Some(store) => super::domains::pairing::cancel(
                    store.as_ref(),
                    self.pairing_notifier.as_deref(),
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
            "nexo/admin/llm_providers/upsert" => match (&self.llm_yaml, &self.reload_signal) {
                (Some(llm), Some(reload)) => {
                    let trigger = reload.clone();
                    super::domains::llm_providers::upsert(
                        llm.as_ref(),
                        params,
                        &move || trigger(),
                    )
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
                    super::domains::channels::approve(
                        yaml.as_ref(),
                        params,
                        &move || trigger(),
                    )
                }
                _ => AdminRpcResult::err(AdminRpcError::Internal(
                    "channels domain not configured".into(),
                )),
            },
            "nexo/admin/channels/revoke" => match (&self.agents_yaml, &self.reload_signal) {
                (Some(yaml), Some(reload)) => {
                    let trigger = reload.clone();
                    super::domains::channels::revoke(
                        yaml.as_ref(),
                        params,
                        &move || trigger(),
                    )
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
                Some(reader) => {
                    super::domains::agent_events::list(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agent_events domain not configured".into(),
                )),
            },
            "nexo/admin/agent_events/read" => match &self.transcript_reader {
                Some(reader) => {
                    super::domains::agent_events::read(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agent_events domain not configured".into(),
                )),
            },
            "nexo/admin/agent_events/search" => match &self.transcript_reader {
                Some(reader) => {
                    super::domains::agent_events::search(reader.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agent_events domain not configured".into(),
                )),
            },
            "nexo/admin/processing/pause" => match &self.processing_store {
                Some(store) => {
                    // Phase 82.14 cross-cut: auto-resolve any
                    // pending escalation matching the same
                    // scope before flipping the pause state.
                    // Best-effort — failures here are logged
                    // but never block the pause itself.
                    if let Some(escalations) = &self.escalation_store {
                        if let Ok(p) = serde_json::from_value::<
                            nexo_tool_meta::admin::processing::ProcessingPauseParams,
                        >(params.clone())
                        {
                            if let Err(e) =
                                super::domains::escalations::auto_resolve_on_pause(
                                    escalations.as_ref(),
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
                    super::domains::processing::pause(store.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "processing domain not configured".into(),
                )),
            },
            "nexo/admin/processing/resume" => match &self.processing_store {
                Some(store) => {
                    super::domains::processing::resume(store.as_ref(), params).await
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
                        params,
                    )
                    .await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "processing domain not configured".into(),
                )),
            },
            "nexo/admin/processing/state" => match &self.processing_store {
                Some(store) => {
                    super::domains::processing::state(store.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "processing domain not configured".into(),
                )),
            },
            "nexo/admin/escalations/list" => match &self.escalation_store {
                Some(store) => {
                    super::domains::escalations::list(store.as_ref(), params).await
                }
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "escalations domain not configured".into(),
                )),
            },
            "nexo/admin/escalations/resolve" => match &self.escalation_store {
                Some(store) => {
                    super::domains::escalations::resolve(store.as_ref(), params).await
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
    async fn audit_writer_records_each_call_with_args_hash() {
        let writer = Arc::new(InMemoryAuditWriter::new());
        let d = dispatcher_granting("agent-creator", &["_echo"])
            .with_audit_writer(writer.clone());
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
        let d = dispatcher_granting("agent-creator", &["_echo"])
            .with_audit_writer(writer.clone());
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
            fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> { Ok(vec![]) }
            fn read_agent_field(&self, _: &str, _: &str) -> anyhow::Result<Option<Value>> { Ok(None) }
            fn upsert_agent_field(&self, _: &str, _: &str, _: Value) -> anyhow::Result<()> { Ok(()) }
            fn remove_agent(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
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
}
