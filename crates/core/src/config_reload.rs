//! Phase 18 — runtime config reload coordinator.
//!
//! Watches the config directory, re-runs boot validation, builds a
//! fresh `RuntimeSnapshot` per agent, and dispatches
//! `ReloadCommand::Apply` to each live runtime through the per-agent
//! mpsc channel the runtime exposed via `reload_sender()`. The
//! existing per-agent snapshot is left in place whenever validation
//! fails or a snapshot cannot be built — servicing never drops to a
//! broken config.
//!
//! Scope (Phase 18): hot-swap of **existing** agents only. Adding a
//! brand-new agent id or removing a running one requires spawn/teardown
//! plumbing that lives in `src/main.rs` today; that is Phase 19.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use dashmap::DashMap;
use nexo_broker::{AnyBroker, BrokerHandle};
use nexo_config::{AppConfig, TelegramPluginConfig};
use nexo_llm::LlmRegistry;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::agent::runtime::ReloadCommand;
use crate::runtime_snapshot::RuntimeSnapshot;
use crate::telemetry;

/// Per-agent state the coordinator needs to dispatch a reload. Held
/// in a `DashMap<String, AgentReloadHandle>` keyed by agent id.
///
/// `known_tools` captures the agent's tool surface at boot time —
/// builtins + plugins + MCP + extensions + skills, after the per-agent
/// allowlist prune. The coordinator uses it during reload so a typo
/// in `allowed_tools` (binding-level) fails the swap instead of
/// silently degrading to "agent has no tools at runtime".
pub struct AgentReloadHandle {
    pub reload_tx: mpsc::Sender<ReloadCommand>,
    pub known_tools: Arc<Vec<String>>,
}

/// Hook the reload coordinator runs after every successful swap.
/// Used to invalidate process-wide caches that hold a stale view of
/// data the reload may have changed (e.g. `PairingGate`'s in-memory
/// allowlist cache, which would otherwise keep blocking a sender the
/// operator just `nexo pair seed`-ed). Hooks are best-effort; they
/// run sequentially under the same gate as the reload itself, so
/// keep them cheap (one mutex / one dashmap clear).
pub type PostReloadHook = Box<dyn Fn() + Send + Sync>;

/// Reload coordinator. One instance per process; `start` spawns the
/// file watcher and the broker `control.reload` subscriber.
pub struct ConfigReloadCoordinator {
    config_dir: PathBuf,
    runtimes: DashMap<String, AgentReloadHandle>,
    llm_registry: Arc<LlmRegistry>,
    version: Mutex<u64>,
    /// Serial gate so two overlapping triggers (watcher + CLI) don't
    /// race the snapshot build. The second trigger queues behind the
    /// first.
    gate: Mutex<()>,
    /// Broker handle attached at `start()`. `Some` once the daemon is
    /// up; the file-watcher branch uses it to publish
    /// `events.runtime.config.reloaded` after every successful swap so
    /// extensions and dashboards can react without polling.
    broker: ArcSwapOption<AnyBroker>,
    /// Cache-flush hooks fired after every successful reload. Locked
    /// by the same gate as the reload to keep the contract simple
    /// (no observer can run mid-swap).
    post_hooks: Mutex<Vec<PostReloadHook>>,
    shutdown: CancellationToken,
}

/// Result returned by [`ConfigReloadCoordinator::reload`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReloadOutcome {
    pub version: u64,
    pub applied: Vec<String>,
    pub rejected: Vec<ReloadRejection>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReloadRejection {
    pub agent_id: Option<String>,
    pub reason: String,
}

impl ConfigReloadCoordinator {
    pub fn new(
        config_dir: PathBuf,
        llm_registry: Arc<LlmRegistry>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            config_dir,
            runtimes: DashMap::new(),
            llm_registry,
            version: Mutex::new(0),
            gate: Mutex::new(()),
            broker: ArcSwapOption::from(None),
            post_hooks: Mutex::new(Vec::new()),
            shutdown,
        }
    }

    /// Register a closure that fires after every successful reload.
    /// Callers should add their cache-invalidation entry point here
    /// at boot. Hooks run inside the reload gate so they observe the
    /// new config but cannot themselves overlap a future reload.
    pub async fn register_post_hook(&self, hook: PostReloadHook) {
        self.post_hooks.lock().await.push(hook);
    }

    /// Register a live agent runtime's reload channel. Called once per
    /// agent during boot; subsequent reloads look these up to dispatch.
    pub fn register(
        &self,
        agent_id: impl Into<String>,
        reload_tx: mpsc::Sender<ReloadCommand>,
        known_tools: Arc<Vec<String>>,
    ) {
        self.runtimes.insert(
            agent_id.into(),
            AgentReloadHandle {
                reload_tx,
                known_tools,
            },
        );
    }

    /// Re-read config, validate, build snapshots, dispatch. Returns an
    /// aggregate outcome — successful ids in `applied`, per-agent or
    /// top-level failures in `rejected`. The coordinator never panics
    /// on a bad config; it logs + bumps the rejected counter and keeps
    /// old snapshots serving.
    pub async fn reload(&self) -> ReloadOutcome {
        let _gate = self.gate.lock().await;
        let started = Instant::now();
        let mut applied: Vec<String> = Vec::new();
        let mut rejected: Vec<ReloadRejection> = Vec::new();

        // 1. Load + env-resolve.
        let cfg = match AppConfig::load(&self.config_dir) {
            Ok(c) => c,
            Err(e) => {
                telemetry::inc_config_reload_rejected();
                tracing::warn!(error = %e, "config reload: load failed, keeping previous snapshot");
                rejected.push(ReloadRejection {
                    agent_id: None,
                    reason: format!("AppConfig::load: {e}"),
                });
                let current = *self.version.lock().await;
                return ReloadOutcome {
                    version: current,
                    applied,
                    rejected,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                };
            }
        };

        // 2. Structural + provider validation (aggregate errors).
        let known_providers = crate::agent::KnownProviders::new(self.llm_registry.names());
        let telegram_instances: &[TelegramPluginConfig] = &cfg.plugins.telegram;
        if let Err(e) = crate::agent::validate_agents_with_providers(
            &cfg.agents.agents,
            telegram_instances,
            &crate::agent::KnownTools::default(),
            &known_providers,
        ) {
            telemetry::inc_config_reload_rejected();
            tracing::warn!(error = %e, "config reload: validation failed, keeping previous snapshot");
            rejected.push(ReloadRejection {
                agent_id: None,
                reason: format!("validation: {e}"),
            });
            let current = *self.version.lock().await;
            return ReloadOutcome {
                version: current,
                applied,
                rejected,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }

        // 3. Bump version + build snapshots per agent present in BOTH
        //    old (registered handles) and new (config). Agents that
        //    disappear or appear are Phase 19 scope — we skip them
        //    with a rejection entry so the operator sees the diff.
        let mut version_guard = self.version.lock().await;
        *version_guard += 1;
        let new_version = *version_guard;
        drop(version_guard);

        for agent_cfg in &cfg.agents.agents {
            let Some(handle) = self.runtimes.get(&agent_cfg.id) else {
                rejected.push(ReloadRejection {
                    agent_id: Some(agent_cfg.id.clone()),
                    reason: "adding a new agent at runtime is not supported in Phase 18; \
                             restart to spawn"
                        .into(),
                });
                continue;
            };

            // Per-agent post-assembly tool-name validation. Mirrors
            // the boot-path second-pass check so a binding's typo'd
            // `allowed_tools` rejects the reload instead of silently
            // landing a config that the runtime then has to translate
            // into a "tool not available" error every turn.
            let known_strs: Vec<&str> = handle.known_tools.iter().map(|s| s.as_str()).collect();
            let catalog = crate::agent::KnownTools::new(known_strs);
            if let Err(e) = crate::agent::validate_agent(agent_cfg, &cfg.plugins.telegram, &catalog)
            {
                rejected.push(ReloadRejection {
                    agent_id: Some(agent_cfg.id.clone()),
                    reason: format!("post-assembly validation: {e}"),
                });
                continue;
            }

            let snap = match RuntimeSnapshot::build(
                Arc::new(agent_cfg.clone()),
                &self.llm_registry,
                &cfg.llm,
                new_version,
            ) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    rejected.push(ReloadRejection {
                        agent_id: Some(agent_cfg.id.clone()),
                        reason: format!("snapshot build: {e}"),
                    });
                    continue;
                }
            };

            match handle.reload_tx.send(ReloadCommand::Apply(snap)).await {
                Ok(()) => applied.push(agent_cfg.id.clone()),
                Err(e) => rejected.push(ReloadRejection {
                    agent_id: Some(agent_cfg.id.clone()),
                    reason: format!("dispatch: {e}"),
                }),
            }
        }

        // 4. Detect removed agents (registered but absent from new cfg).
        for entry in self.runtimes.iter() {
            let id = entry.key();
            if !cfg.agents.agents.iter().any(|a| &a.id == id) {
                rejected.push(ReloadRejection {
                    agent_id: Some(id.clone()),
                    reason: "removing an agent at runtime is not supported in Phase 18; \
                             restart to drop"
                        .into(),
                });
            }
        }

        let elapsed_ms = started.elapsed().as_millis() as u64;
        telemetry::observe_config_reload_latency_ms(elapsed_ms);

        if !applied.is_empty() {
            telemetry::inc_config_reload_applied();
            tracing::info!(
                version = new_version,
                applied = ?applied,
                rejected_count = rejected.len(),
                elapsed_ms,
                "config reload applied",
            );
            // Run cache-invalidation hooks (e.g. PairingGate flush)
            // before publishing the reload event, so consumers see
            // the new state cleanly. We hold the lock briefly — the
            // gate above already prevents overlapping reloads, the
            // post-hooks lock just guards the registration list.
            let hooks = self.post_hooks.lock().await;
            for hook in hooks.iter() {
                hook();
            }
            drop(hooks);
            // Phase 18 follow-up — broadcast the event so extensions
            // and dashboards can react without polling. Non-fatal if
            // the publish fails; the metrics + log already record the
            // swap.
            if let Some(broker) = self.broker.load_full() {
                let payload = serde_json::json!({
                    "version": new_version,
                    "applied": &applied,
                    "rejected": &rejected,
                    "elapsed_ms": elapsed_ms,
                });
                let topic = "events.runtime.config.reloaded";
                let evt = nexo_broker::Event::new(topic, "config-reload", payload);
                if let Err(e) = broker.publish(topic, evt).await {
                    tracing::warn!(error = %e, "failed to publish events.runtime.config.reloaded");
                }
            }
        }
        if !rejected.is_empty() {
            tracing::warn!(
                version = new_version,
                rejected = ?rejected,
                "config reload: partial rejects",
            );
        }

        ReloadOutcome {
            version: new_version,
            applied,
            rejected,
            elapsed_ms,
        }
    }

    /// Start the watcher + broker subscriber. Returns immediately; the
    /// work runs on spawned tasks that honour `self.shutdown`.
    pub async fn start(
        self: Arc<Self>,
        broker: AnyBroker,
        reload: nexo_config::RuntimeReloadConfig,
    ) -> anyhow::Result<()> {
        if !reload.enabled {
            tracing::info!("config hot-reload disabled via runtime.yaml");
            return Ok(());
        }

        // Stash the broker so reload() can emit
        // `events.runtime.config.reloaded` regardless of whether the
        // trigger came from the file watcher or the CLI.
        self.broker.store(Some(Arc::new(broker.clone())));

        // File watcher → debounced notifications.
        let watcher_rx = crate::config_watch::spawn_config_watcher(
            self.config_dir.clone(),
            reload.extra_watch_paths.clone(),
            Duration::from_millis(reload.debounce_ms),
            self.shutdown.clone(),
        )?;
        let coord_watcher = Arc::clone(&self);
        tokio::spawn(async move {
            let mut rx = watcher_rx;
            while let Some(()) = rx.recv().await {
                if coord_watcher.shutdown.is_cancelled() {
                    break;
                }
                let _ = coord_watcher.reload().await;
            }
        });

        // Broker subscriber — manual triggers from `agent reload`.
        let mut sub = broker.subscribe("control.reload").await?;
        let coord_broker = Arc::clone(&self);
        let broker_clone = broker.clone();
        tokio::spawn(async move {
            loop {
                if coord_broker.shutdown.is_cancelled() {
                    break;
                }
                let Some(_event) = sub.next().await else {
                    break;
                };
                let outcome = coord_broker.reload().await;
                let ack_topic = "control.reload.ack";
                let payload = serde_json::to_value(&outcome)
                    .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));
                let evt = nexo_broker::Event::new(ack_topic, "config-reload", payload);
                if let Err(e) = broker_clone.publish(ack_topic, evt).await {
                    tracing::warn!(error = %e, "failed to publish control.reload.ack");
                }
            }
        });

        Ok(())
    }

    /// Current monotonic version (for telemetry / tests).
    pub async fn version(&self) -> u64 {
        *self.version.lock().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reload_with_no_config_dir_rejects_cleanly() {
        let coord = Arc::new(ConfigReloadCoordinator::new(
            PathBuf::from("/nonexistent-config-dir-xyz"),
            Arc::new(LlmRegistry::with_builtins()),
            CancellationToken::new(),
        ));
        let outcome = coord.reload().await;
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.rejected.len(), 1);
        assert!(outcome.rejected[0].agent_id.is_none());
        assert!(outcome.rejected[0].reason.contains("AppConfig::load"));
    }

    #[tokio::test]
    async fn version_starts_at_zero() {
        let coord = ConfigReloadCoordinator::new(
            PathBuf::from("."),
            Arc::new(LlmRegistry::with_builtins()),
            CancellationToken::new(),
        );
        assert_eq!(coord.version().await, 0);
    }

    #[tokio::test]
    async fn post_hooks_register_and_can_be_invoked_in_order() {
        // Phase 70.7 — verify the hook list grows and runs in
        // registration order. The reload() success path that fires
        // them needs a full AppConfig on disk; that's covered by the
        // boot smoke tests. Here we just check the storage / FIFO.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let coord = ConfigReloadCoordinator::new(
            PathBuf::from("."),
            Arc::new(LlmRegistry::with_builtins()),
            CancellationToken::new(),
        );
        let order = Arc::new(AtomicUsize::new(0));
        let a_witness = Arc::new(AtomicUsize::new(0));
        let b_witness = Arc::new(AtomicUsize::new(0));
        {
            let order = Arc::clone(&order);
            let w = Arc::clone(&a_witness);
            coord
                .register_post_hook(Box::new(move || {
                    w.store(order.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                }))
                .await;
        }
        {
            let order = Arc::clone(&order);
            let w = Arc::clone(&b_witness);
            coord
                .register_post_hook(Box::new(move || {
                    w.store(order.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                }))
                .await;
        }
        let hooks = coord.post_hooks.lock().await;
        assert_eq!(hooks.len(), 2);
        for hook in hooks.iter() {
            hook();
        }
        drop(hooks);
        assert_eq!(a_witness.load(Ordering::SeqCst), 1);
        assert_eq!(b_witness.load(Ordering::SeqCst), 2);
    }
}
