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
use nexo_core::agent::admin_rpc::{
    validate_capabilities_at_boot, AdminAuditWriter, AdminCapabilityDecl, AdminRpcDispatcher,
    CapabilitySet, DispatcherAdminRouter, InMemoryAuditWriter, SqliteAdminAuditWriter,
};
use nexo_core::agent::admin_rpc::dispatcher::ReloadSignal;
use nexo_extensions::runtime::admin_router::SharedAdminRouter;
use nexo_extensions::runtime::stdio::StdioSpawnOptions;
use nexo_plugin_manifest::manifest::AdminCapabilities;
use tokio::task::JoinHandle;

use crate::admin_adapters::{
    AgentsYamlPatcher, DeferredAdminOutboundWriter, FilesystemCredentialStore,
    InMemoryPairingChallengeStore, LlmYamlPatcherFs,
};

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
    }
}

/// Errors surfaced from boot. A required-capability mismatch is
/// fail-fast — the operator misconfigured the grant matrix and
/// the microapp would otherwise crash on first call.
#[derive(Debug, thiserror::Error)]
pub enum AdminBootstrapError {
    #[error("admin capability boot validation failed: {0}")]
    CapabilityValidation(String),
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
    /// Phase 18 reload signal — invoked by domain handlers
    /// after a successful yaml mutation.
    pub reload_signal: ReloadSignal,
}

impl AdminRpcBootstrap {
    /// Build every per-microapp wire. Returns `Ok(None)` when no
    /// extension declares admin capabilities — the daemon then
    /// runs without any admin RPC plumbing (zero overhead).
    pub async fn build(
        inputs: AdminBootstrapInputs<'_>,
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

        // Build one (router, deferred writer) per declared microapp.
        // Pairing notifier is intentionally omitted for v1: the
        // notifier needs an extra wire from boot's `outbox_tx` to
        // the same sender the deferred writer drains, which is
        // chicken-and-egg (sender created inside `spawn_with`).
        // Microapps fall back to polling `pairing/status` until a
        // future follow-up exposes a separate notification queue
        // independent of the response writer.
        let mut wires = BTreeMap::new();
        for (id, _decl) in &declared {
            let writer = Arc::new(DeferredAdminOutboundWriter::new());

            let dispatcher = AdminRpcDispatcher::new()
                .with_capabilities(capability_set.clone())
                .with_audit_writer(audit.clone())
                .with_agents_domain(agents_yaml.clone(), inputs.reload_signal.clone())
                .with_credentials_domain(credential_store.clone())
                .with_pairing_domain(pairing_store.clone(), None)
                .with_llm_providers_domain(llm_yaml.clone())
                .with_channels_domain();

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
        }))
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

    /// `true` when at least one microapp has an admin wire.
    pub fn is_active(&self) -> bool {
        !self.wires.is_empty()
    }

    /// Microapp ids carrying an admin wire — for boot diagnostics.
    pub fn wired_ids(&self) -> Vec<String> {
        self.wires.keys().cloned().collect()
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
            reload_signal: noop_reload(),
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
            reload_signal: noop_reload(),
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
            reload_signal: noop_reload(),
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
}
