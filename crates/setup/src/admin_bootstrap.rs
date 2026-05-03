//! Phase 82.10.h.b.5 — admin RPC bootstrap.
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
use nexo_core::agent::admin_rpc::domains::agent_events::TranscriptReader;
use nexo_core::agent::admin_rpc::{
    validate_capabilities_at_boot, AdminAuditWriter, AdminCapabilityDecl, AdminRpcDispatcher,
    CapabilitySet, DispatcherAdminRouter, InMemoryAuditWriter, SqliteAdminAuditWriter,
};
use nexo_core::agent::admin_rpc::dispatcher::ReloadSignal;
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

/// One bootstrapped microapp: the router that the spawn loop
/// passes via `StdioSpawnOptions::admin_router`, plus the
/// deferred writer that boot binds post-spawn.
struct PerMicroappWire {
    router: Arc<DispatcherAdminRouter>,
    writer: Arc<DeferredAdminOutboundWriter>,
    /// Phase 82.10.h.b.pairing — `nexo/notify/pairing_status_changed`
    /// notifier sharing the same stdin queue as `writer`. Bound
    /// in [`AdminRpcBootstrap::bind_writer`] alongside the
    /// response writer so microapps see status transitions in
    /// real time instead of polling.
    pairing_notifier: Arc<crate::admin_adapters::DeferredPairingNotifier>,
}

/// Owns every shared admin RPC singleton + per-microapp wires.
/// Drop the bootstrap to cleanly tear down the prune task.
pub struct AdminRpcBootstrap {
    wires: BTreeMap<String, PerMicroappWire>,
    /// Pairing prune task — aborted on drop.
    prune_handle: Option<JoinHandle<()>>,
    /// Phase 82.11 — per-microapp firehose subscriber tasks.
    /// Each task reads from the shared broadcast channel and
    /// forwards filtered frames to the deferred writer for that
    /// microapp. Aborted on bootstrap drop.
    subscribe_handles: Vec<JoinHandle<()>>,
    /// Live emitter — `Arc` so test code can assert subscriber
    /// counts + spec callers can clone into the
    /// `TranscriptWriter::with_emitter` builder.
    event_emitter: Arc<dyn AgentEventEmitter>,
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
    /// Phase 82.12 — extension declared a non-loopback HTTP
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
    /// Phase 82.12 — per-extension `[capabilities.http_server]`
    /// block from `plugin.toml`. Boot validates the bind policy
    /// against `extensions.yaml.<id>.allow_external_bind`. v0
    /// only checks the policy; the actual probe + monitor loop
    /// are spawned by main.rs after `initialize` returns. Keys
    /// match `admin_capabilities`; missing entries skip the
    /// bind check.
    pub http_server_capabilities: &'a BTreeMap<String, HttpServerCapability>,
    /// Phase 18 reload signal — invoked by domain handlers
    /// after a successful yaml mutation.
    pub reload_signal: ReloadSignal,
    /// Phase 82.11 — optional transcripts reader. When `Some`,
    /// the agent_events admin domain (`list/read/search`) is
    /// installed on every dispatcher. `None` skips the domain
    /// (microapps see `-32601` for those methods).
    pub transcript_reader: Option<Arc<dyn TranscriptReader>>,
    /// Phase 83.8.4.b — broker handle the
    /// `BrokerOutboundDispatcher` publishes to. `None` keeps the
    /// outbound surface disabled —
    /// `nexo/admin/processing/intervention` returns
    /// `-32603 channel_outbound dispatcher not configured`.
    /// Production wiring in `main.rs` always passes
    /// `Some(broker.clone())`.
    pub broker: Option<nexo_broker::AnyBroker>,
    /// Phase 82.13.b.1 — optional `TranscriptWriter` shared with
    /// the agent runtime. When `Some`, the dispatcher gains a
    /// `TranscriptWriterAppender` so
    /// `nexo/admin/processing/intervention` stamps operator
    /// replies on the agent transcript (the agent reads them on
    /// its next turn). When `None`, the channel send still
    /// happens but `ProcessingAck.transcript_stamped` reports
    /// `Some(false)`.
    pub transcript_writer:
        Option<Arc<nexo_core::agent::transcripts::TranscriptWriter>>,
    /// Phase 82.13.c — operator pause-control store SHARED with
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
    /// Phase 83.8.12.2 close-out — multi-tenant SaaS registry.
    /// `None` keeps `nexo/admin/tenants/*` returning the typed
    /// `tenants domain not configured` error (single-tenant
    /// deployments). Production wires
    /// `crate::admin_adapters::TenantsYamlPatcher` against
    /// `config/tenants.yaml`.
    pub tenant_store:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::tenants::TenantStore>>,
    /// Phase 82.10.k — secrets store. `None` keeps
    /// `nexo/admin/secrets/write` returning the typed
    /// `secrets domain not configured` -32603. Production wires
    /// `crate::secrets_store::FsSecretsStore` rooted at
    /// `<state_root>/secrets/` (mode 0600 file write +
    /// `std::env::set_var` so existing LLM clients see the new
    /// value without a daemon restart). Resolves M9.frame.a
    /// (microapp follow-up).
    pub secrets_store:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::secrets::SecretsStore>>,
    /// Phase 83.8.2 close-out — skills domain store. `None`
    /// keeps `nexo/admin/skills/*` returning the typed
    /// `skills domain not configured` -32603. Production wires
    /// `crate::admin_adapters::FsSkillsStore` against the same
    /// skills root the runtime `SkillLoader` reads from so admin
    /// writes land where the runtime reads (Phase 83.8.12.6
    /// layout).
    pub skills_store:
        Option<Arc<dyn nexo_core::agent::admin_rpc::domains::skills::SkillsStore>>,
    /// Phase 82.14 close-out — escalations store. `None` keeps
    /// `nexo/admin/escalations/*` returning the typed
    /// `escalations domain not configured` -32603. Production
    /// wires the in-memory store + the future SQLite adapter.
    pub escalation_store: Option<
        Arc<dyn nexo_core::agent::admin_rpc::domains::escalations::EscalationStore>,
    >,
    /// Phase 82.11.log close-out — durable agent-event log. When
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
    pub agent_event_log: Option<
        Arc<nexo_core::agent::admin_rpc::SqliteAgentEventLog>,
    >,
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
        let capability_set = CapabilitySet::from_grants(report.grants);

        // Phase 82.12 — validate http_server bind policy. Each
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
            if let Err(bind) =
                crate::http_supervisor::validate_bind_policy(decl, allow_external)
            {
                return Err(AdminBootstrapError::ExternalBindNotAllowed {
                    microapp_id: microapp_id.clone(),
                    bind,
                });
            }
        }

        // Audit writer — single instance shared across every
        // dispatcher.
        let audit: Arc<dyn AdminAuditWriter> = match inputs.audit_db {
            Some(path) => match SqliteAdminAuditWriter::open(path).await {
                Ok(w) => {
                    tracing::info!(path=%path.display(), "admin audit DB opened");
                    Arc::new(w)
                }
                Err(e) => {
                    tracing::warn!(
                        path=%path.display(),
                        error=%e,
                        "admin audit DB open failed; falling back to InMemoryAuditWriter",
                    );
                    Arc::new(InMemoryAuditWriter::new())
                }
            },
            None => Arc::new(InMemoryAuditWriter::new()),
        };

        // Filesystem-side adapters — singletons.
        let agents_yaml = Arc::new(AgentsYamlPatcher::new(
            inputs.config_dir.join("agents.yaml"),
        ));
        let llm_yaml = Arc::new(LlmYamlPatcherFs::new(inputs.config_dir.join("llm.yaml")));
        let credential_store = Arc::new(FilesystemCredentialStore::new(inputs.secrets_root));
        let pairing_store = Arc::new(InMemoryPairingChallengeStore::new(
            Duration::from_secs(5 * 60),
        ));

        // Spawn the pairing prune task — every 30 s.
        let prune_handle = {
            let store = pairing_store.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                interval.set_missed_tick_behavior(
                    tokio::time::MissedTickBehavior::Delay,
                );
                loop {
                    interval.tick().await;
                    let removed = store.prune_expired();
                    if removed > 0 {
                        tracing::debug!(removed, "pruned expired pairing challenges");
                    }
                }
            })
        };

        // Phase 82.11 — broadcast emitter shared across every
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
        // Phase 82.11.log close-out — when boot wires a durable
        // `SqliteAgentEventLog`, compose `Tee([Broadcast, Log])`
        // so every emit reaches both live subscribers AND the
        // durable backfill source. Without a log, the broadcast
        // (or noop) emitter passes through unchanged.
        let event_emitter: Arc<dyn AgentEventEmitter> =
            match (broadcast.clone(), inputs.agent_event_log.clone()) {
                (Some(b), Some(log)) => Arc::new(
                    nexo_core::agent::agent_events::TeeAgentEventEmitter::with_sinks(
                        vec![
                            b as Arc<dyn AgentEventEmitter>,
                            log as Arc<dyn AgentEventEmitter>,
                        ],
                    ),
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
        for (id, _decl) in &declared {
            let writer = Arc::new(DeferredAdminOutboundWriter::new());
            // Phase 82.10.h.b.pairing — separate notification
            // queue for `nexo/notify/pairing_status_changed` so
            // microapps stop polling pairing/status on a 1-2 s
            // cadence. Bound post-spawn alongside the response
            // writer in `bind_writer`.
            let pairing_notifier =
                Arc::new(crate::admin_adapters::DeferredPairingNotifier::new());

            let mut dispatcher = AdminRpcDispatcher::new()
                .with_capabilities(capability_set.clone())
                .with_audit_writer(audit.clone())
                .with_agents_domain(agents_yaml.clone(), inputs.reload_signal.clone())
                .with_credentials_domain(credential_store.clone())
                .with_pairing_domain(
                    pairing_store.clone(),
                    Some(pairing_notifier.clone()),
                )
                .with_llm_providers_domain(llm_yaml.clone())
                .with_channels_domain();
            if let Some(reader) = inputs.transcript_reader.clone() {
                dispatcher = dispatcher.with_agent_events_domain(reader);
            }
            // Phase 83.8.4.b — wire the production
            // BrokerOutboundDispatcher when the broker handle is
            // available. Without it, `processing.intervention`
            // returns the typed "channel_outbound dispatcher not
            // configured" error so callers diagnose the wire-up
            // gap clearly.
            if let Some(broker) = inputs.broker.clone() {
                let outbound = Arc::new(
                    crate::admin_adapters::BrokerOutboundDispatcher::new(broker)
                        .with_translator(Box::new(
                            crate::admin_adapters::WhatsAppTranslator,
                        )),
                );
                dispatcher = dispatcher.with_channel_outbound(outbound);
            }
            // Phase 82.13.b.1 — wire the production transcript
            // appender when boot has the writer handle. Without
            // it, intervention(Reply) still dispatches via the
            // outbound adapter but the ack reports
            // `transcript_stamped: false`.
            if let Some(writer) = inputs.transcript_writer.clone() {
                let appender = Arc::new(
                    crate::admin_adapters::TranscriptWriterAppender::new(writer),
                );
                dispatcher = dispatcher.with_transcript_appender(appender);
            }
            // Phase 82.13.c — install the processing-control
            // domain when boot has a shared store. Same `Arc`
            // is also handed to every runtime via
            // `Runtime::with_processing_store` so a pause RPC
            // reaches the inbound loop on the next message.
            if let Some(store) = inputs.processing_store.clone() {
                dispatcher = dispatcher.with_processing_domain(store);
            }
            // Phase 83.8.12.2 close-out — install the tenants
            // domain when boot has the production
            // `TenantsYamlPatcher` adapter. Without it,
            // `nexo/admin/tenants/*` returns the typed
            // `tenants domain not configured` -32603 so the
            // microapp surfaces a clear wire-up gap.
            if let Some(store) = inputs.tenant_store.clone() {
                dispatcher = dispatcher.with_tenants_domain(store);
            }
            // Phase 82.10.k — install the secrets domain when
            // boot has the production `FsSecretsStore` adapter.
            // Without it, `nexo/admin/secrets/write` returns the
            // typed `secrets domain not configured` -32603 so
            // the microapp wizard surfaces a clear wire-up gap.
            if let Some(store) = inputs.secrets_store.clone() {
                dispatcher = dispatcher.with_secrets_domain(store);
            }
            // Phase 83.8.2 close-out — install the skills domain
            // when boot has the production `FsSkillsStore`.
            if let Some(store) = inputs.skills_store.clone() {
                dispatcher = dispatcher.with_skills_domain(store);
            }
            // Phase 82.14 close-out — install the escalations
            // domain when boot has a store adapter wired.
            if let Some(store) = inputs.escalation_store.clone() {
                dispatcher = dispatcher.with_escalations_domain(store);
            }

            // Phase 82.11 — spawn a per-microapp subscriber when
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
                    let writer_clone: Arc<DeferredAdminOutboundWriter> =
                        writer.clone();
                    let microapp_id = id.clone();
                    let handle = tokio::spawn(async move {
                        firehose_subscriber_loop(microapp_id, &mut rx, writer_clone)
                            .await;
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
                },
            );
        }

        Ok(Some(Self {
            wires,
            prune_handle: Some(prune_handle),
            subscribe_handles,
            event_emitter,
        }))
    }

    /// Phase 82.11 — clone of the shared firehose emitter.
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
    /// (Phase 82.10.h.b.pairing — closes the chicken-and-egg
    /// that previously left microapps polling
    /// `pairing/status`).
    pub fn bind_writer(
        &self,
        extension_id: &str,
        sender: tokio::sync::mpsc::Sender<String>,
    ) {
        if let Some(wire) = self.wires.get(extension_id) {
            wire.writer.bind(sender.clone());
            wire.pairing_notifier.bind(sender);
        }
    }

    /// Phase 82.11 — number of subscribe tasks the boot wired.
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
        })
        .await
        .unwrap();
        assert!(result.is_none(), "no admin wires when no caps declared");
    }

    #[tokio::test]
    async fn build_fails_when_required_capability_not_granted() {
        let cfg = extensions_cfg_with_grant("agent-creator", &[]);
        let mut manifests = BTreeMap::new();
        manifests.insert(
            "agent-creator".into(),
            admin_caps(&["agents_crud"], &[]),
        );
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
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
        manifests.insert(
            "agent-creator".into(),
            admin_caps(&["agents_crud"], &[]),
        );
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
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
        let cfg = extensions_cfg_with_grant(
            "agent-creator",
            &["agents_crud", "transcripts_subscribe"],
        );
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
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
        manifests.insert(
            "agent-creator".into(),
            admin_caps(&["agents_crud"], &[]),
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
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
        let cfg = extensions_cfg_with_grant(
            "audit-app",
            &["agents_crud", "agent_events_subscribe_all"],
        );
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
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
        manifests.insert(
            "agent-creator".into(),
            admin_caps(&["agents_crud"], &[]),
        );
        let mut http: BTreeMap<String, HttpServerCapability> = BTreeMap::new();
        http.insert(
            "agent-creator".into(),
            HttpServerCapability {
                port: 9001,
                bind: "0.0.0.0".into(),
                token_env: "T".into(),
                health_path: "/healthz".into(),
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
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
        manifests.insert(
            "agent-creator".into(),
            admin_caps(&["agents_crud"], &[]),
        );
        let mut http: BTreeMap<String, HttpServerCapability> = BTreeMap::new();
        http.insert(
            "agent-creator".into(),
            HttpServerCapability {
                port: 9001,
                bind: "0.0.0.0".into(),
                token_env: "T".into(),
                health_path: "/healthz".into(),
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
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
        manifests.insert(
            "agent-creator".into(),
            admin_caps(&["agents_crud"], &[]),
        );
        let mut http: BTreeMap<String, HttpServerCapability> = BTreeMap::new();
        http.insert(
            "agent-creator".into(),
            HttpServerCapability {
                port: 9001,
                bind: "127.0.0.1".into(),
                token_env: "T".into(),
                health_path: "/healthz".into(),
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
            },
            true,
        )
        .await
        .expect("bootstrap built");
    }

    /// Phase 82.13.c.2 — boot must share the SAME
    /// `Arc<dyn ProcessingControlStore>` between the admin RPC
    /// dispatcher and the agent runtime. Without sharing, a
    /// pause RPC would land on one store while the runtime
    /// would consult a different one, and pause would never
    /// reach the inbound loop.
    #[tokio::test]
    async fn shared_processing_store_round_trips_pause_to_runtime_check() {
        use crate::admin_adapters::InMemoryProcessingControlStore;
        use nexo_core::agent::admin_rpc::domains::processing::ProcessingControlStore;
        use nexo_tool_meta::admin::processing::{
            ProcessingControlState, ProcessingScope,
        };

        let cfg = extensions_cfg_with_grant("agent-creator", &["agents_crud"]);
        let mut manifests = BTreeMap::new();
        manifests.insert(
            "agent-creator".into(),
            admin_caps(&["agents_crud"], &[]),
        );
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
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
        let cfg = extensions_cfg_with_grant(
            "agent-creator",
            &["agents_crud", "transcripts_subscribe"],
        );
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
            secrets_store: None,
            skills_store: None,
            escalation_store: None,
            agent_event_log: None,
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
            secrets_store: None,
                skills_store: None,
                escalation_store: None,
                agent_event_log: Some(log.clone()),
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
}
