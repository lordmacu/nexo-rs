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
        let event_emitter: Arc<dyn AgentEventEmitter> = match broadcast.clone() {
            Some(b) => b,
            None => Arc::new(NoopAgentEventEmitter),
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

            let mut dispatcher = AdminRpcDispatcher::new()
                .with_capabilities(capability_set.clone())
                .with_audit_writer(audit.clone())
                .with_agents_domain(agents_yaml.clone(), inputs.reload_signal.clone())
                .with_credentials_domain(credential_store.clone())
                .with_pairing_domain(pairing_store.clone(), None)
                .with_llm_providers_domain(llm_yaml.clone())
                .with_channels_domain();
            if let Some(reader) = inputs.transcript_reader.clone() {
                dispatcher = dispatcher.with_agent_events_domain(reader);
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
    /// dispatcher flows out through the extension's stdin.
    pub fn bind_writer(
        &self,
        extension_id: &str,
        sender: tokio::sync::mpsc::Sender<String>,
    ) {
        if let Some(wire) = self.wires.get(extension_id) {
            wire.writer.bind(sender);
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
            },
            true,
        )
        .await
        .expect("bootstrap built");
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
}
