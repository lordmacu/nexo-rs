//! Phase 81.9 — atomic boot helper that runs the
//! discover → merge_agents → merge_skills → init_loop pipeline and
//! returns a single struct holding every handle the per-agent boot
//! loop needs (registry, skill_roots, channel adapter registry).
//!
//! Replaces the `Phase 81.5.b` snippet at `src/main.rs:1928-1954` —
//! same observable behavior, but the four sub-phase handles
//! (81.5 / 81.6 / 81.7 / 81.8) are folded into one call site.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use semver::Version;

use nexo_config::AgentsConfig;

use std::collections::BTreeSet;

use super::capability_aggregator::{
    aggregate_plugin_gates, AggregatedGate, UnmetRequirement,
};
use super::factory::PluginFactoryRegistry;
use super::init_loop::run_plugin_init_loop_with_factory;
use super::{
    discover, merge_plugin_contributed_agents, merge_plugin_contributed_skills,
    run_plugin_init_loop, NexoPluginRegistry, NexoPluginRegistrySnapshot,
    PluginDiscoveryConfig,
};
use crate::agent::channel_adapter::ChannelAdapterRegistry;
use crate::agent::plugin_host::NexoPlugin;
use crate::config_reload::ConfigReloadCoordinator;

/// Phase 81.9 — bundle of registry handles produced by
/// [`wire_plugin_registry`] and consumed by the per-agent boot loop.
pub struct WirePluginRegistryOutput {
    /// Active snapshot container. Boot keeps the `Arc` alive so
    /// later sub-phases can swap (Phase 18 hot-reload, Phase 81.11
    /// admin RPC).
    pub registry: Arc<NexoPluginRegistry>,
    /// Plugin-contribution skill roots, sorted by plugin id
    /// (BTreeMap iteration). Per-agent
    /// `LlmAgentBehavior::with_plugin_skill_roots` consumes this
    /// verbatim; each `SkillLoader::with_plugin_roots(...)` call
    /// uses the same vec so plugin priority is consistent across
    /// agents.
    pub skill_roots: Vec<PathBuf>,
    /// Channel adapter registry. Empty on this phase — no producer
    /// constructs `Arc<dyn ChannelAdapter>` instances yet
    /// (manifest-driven factory lands in Phase 81.12). Held in
    /// scope so the outbound dispatcher refactor (Phase 81.9.b)
    /// can pull it without changing the boot wire shape.
    pub channel_adapter_registry: Arc<ChannelAdapterRegistry>,
    /// Phase 81.11 — plugin-declared capability gates aggregated
    /// at boot. Same data as
    /// `registry.snapshot().last_report.plugin_capability_gates`,
    /// surfaced here for callers that don't want to load the
    /// snapshot.
    pub plugin_capability_gates: std::collections::BTreeMap<String, AggregatedGate>,
    /// Phase 81.11 — capabilities a plugin declared in
    /// `requires.nexo_capabilities` that are not currently wired
    /// in the daemon.
    pub unmet_required_capabilities: Vec<UnmetRequirement>,
}

/// Phase 81.9 — atomic 4-step plugin registry boot wire.
///
/// 1. `discover` walks the search paths in `discovery_cfg`.
/// 2. `merge_plugin_contributed_agents` folds plugin agents into
///    `cfg.agents`, honoring operator priority + per-plugin
///    `allow_override`.
/// 3. `merge_plugin_contributed_skills` catalogs plugin skill
///    roots; they land on the snapshot's `skill_roots` field.
/// 4. `run_plugin_init_loop` exercises any
///    `Arc<dyn NexoPlugin>` handles registered by 81.7.b / 81.12
///    (today: empty map → every plugin records
///    `InitOutcome::NoHandle`).
///
/// All three reports are folded into the snapshot's `last_report`
/// + the snapshot's `skill_roots` is populated. Single
/// `tracing::info!(target: "plugins.discovery")` summary emitted
/// at end with eight fields.
pub fn wire_plugin_registry(
    cfg: &mut AgentsConfig,
    discovery_cfg: &PluginDiscoveryConfig,
    current_version: &Version,
    core_env_vars: &[(&str, &str)],
    available_capabilities: &BTreeSet<String>,
    factory_registry: Option<&PluginFactoryRegistry>,
) -> WirePluginRegistryOutput {
    // 1. discover.
    let snap = discover(discovery_cfg, current_version);

    // 2. merge plugin-contributed agents into the runtime config.
    let agent_merge = merge_plugin_contributed_agents(&snap, cfg);

    // 3. catalog plugin-contributed skill roots.
    let skill_merge = merge_plugin_contributed_skills(&snap);
    let skill_roots: Vec<PathBuf> = skill_merge.skill_roots.values().cloned().collect();

    // 4. drive NexoPlugin::init() per discovered plugin.
    //    Phase 81.12.0 — when the operator supplies a
    //    `factory_registry`, plugins with a registered factory
    //    are instantiated + their `init()` is called; the rest
    //    record NoHandle. With `None` (today's main.rs path),
    //    behavior matches the pre-81.12.0 branch: every plugin
    //    records NoHandle. ctx_factory closure is `unreachable!`
    //    because no current factory's init body actually consumes
    //    a real `PluginInitContext` until 81.12.a-e ship.
    let init_outcomes = match factory_registry {
        Some(factory) => {
            let fut = run_plugin_init_loop_with_factory(
                &snap,
                factory,
                |_id| -> crate::agent::plugin_host::PluginInitContext<'_> {
                    unreachable!(
                        "wire_plugin_registry's ctx_factory not yet wired for real plugins; 81.12.a-e populate this"
                    )
                },
            );
            futures::executor::block_on(fut)
        }
        None => {
            let empty_handles: BTreeMap<String, Arc<dyn NexoPlugin>> = BTreeMap::new();
            let fut = run_plugin_init_loop(
                &snap,
                &empty_handles,
                |_id| -> crate::agent::plugin_host::PluginInitContext<'_> {
                    unreachable!(
                        "wire_plugin_registry passes empty handles; ctx_factory must not be invoked"
                    )
                },
            );
            futures::executor::block_on(fut)
        }
    };

    // Fold everything into a fresh snapshot.
    let mut updated_report = snap.last_report.clone();
    updated_report.fold_agent_merge(agent_merge);
    updated_report.fold_skill_merge(skill_merge);
    updated_report.fold_init_outcomes(init_outcomes);

    // Phase 81.11 — aggregate plugin capability gates + check
    // unmet `requires.nexo_capabilities`. Snapshot for the
    // aggregator borrows the just-discovered plugins (the same
    // ones that will populate the final snapshot below).
    let aggregator_snap_view = NexoPluginRegistrySnapshot {
        plugins: snap.plugins.clone(),
        last_report: super::report::PluginDiscoveryReport::default(),
        skill_roots: BTreeMap::new(),
    };
    let aggregation =
        aggregate_plugin_gates(&aggregator_snap_view, core_env_vars, available_capabilities);
    let plugin_capability_gates_for_output = aggregation.gates.clone();
    let unmet_required_for_output = aggregation.unmet_required.clone();
    updated_report.fold_capability_aggregation(aggregation);

    let new_skill_roots_for_snapshot: BTreeMap<String, PathBuf> = snap
        .plugins
        .iter()
        .filter_map(|p| {
            updated_report
                .contributed_skills_per_plugin
                .get(&p.manifest.plugin.id)
                .map(|_| {
                    let dir = p.root_dir.join(
                        p.manifest
                            .plugin
                            .skills
                            .contributes_dir
                            .clone()
                            .unwrap_or_default(),
                    );
                    (p.manifest.plugin.id.clone(), dir)
                })
        })
        .collect();

    let final_snap = Arc::new(NexoPluginRegistrySnapshot {
        plugins: snap.plugins.clone(),
        last_report: updated_report,
        skill_roots: new_skill_roots_for_snapshot,
    });

    let registry = NexoPluginRegistry::empty();
    registry.swap(final_snap.clone());

    let report = &final_snap.last_report;
    let contributed_agents_total: usize = report
        .contributed_agents_per_plugin
        .values()
        .map(|v| v.len())
        .sum();
    let contributed_skills_total: usize = report
        .contributed_skills_per_plugin
        .values()
        .map(|v| v.len())
        .sum();
    let init_failed_total: usize = report
        .init_outcomes
        .values()
        .filter(|o| matches!(o, super::InitOutcome::Failed { .. }))
        .count();
    tracing::info!(
        target: "plugins.discovery",
        loaded = report.loaded_ids.len(),
        invalid = report.invalid,
        disabled = report.disabled,
        duplicates = report.duplicates,
        contributed_agents_total = contributed_agents_total,
        contributed_skills_total = contributed_skills_total,
        merge_conflicts_total = report.agent_merge_conflicts.len()
            + report.skill_conflicts.len(),
        init_failed_total = init_failed_total,
        "plugin registry wire complete"
    );

    WirePluginRegistryOutput {
        registry,
        skill_roots,
        channel_adapter_registry: Arc::new(ChannelAdapterRegistry::new()),
        plugin_capability_gates: plugin_capability_gates_for_output,
        unmet_required_capabilities: unmet_required_for_output,
    }
}

/// Phase 81.10 — register a post-reload hook with the
/// `ConfigReloadCoordinator` that re-runs `discover()` and
/// atomically swaps the registry snapshot, so a daemon picks up
/// new / removed plugin manifests without restart.
///
/// Captured state (immutable per closure):
/// - `Arc<NexoPluginRegistry>` — the swap target.
/// - `PluginDiscoveryConfig` — boot-time snapshot of the operator's
///   `plugins.discovery` config. Reloads do NOT update this; if the
///   operator wants new search_paths picked up, daemon restart is
///   required (Phase 81.10.b lifts this).
/// - `Version` — daemon's `CARGO_PKG_VERSION` for `min_nexo_version`
///   matching during re-discover.
///
/// Hook body is sync (matches `PostReloadHook`); `discover()` is
/// sync. Errors swallowed via `tracing::warn` per the coord's
/// best-effort contract — the hook never aborts the reload.
///
/// **Limitations** (deferred):
/// - The new snapshot's `skill_roots` field is left empty. Running
///   agents already cloned their `LlmAgentBehavior.plugin_skill_roots`
///   at boot — re-merging skill roots here would not affect them.
///   Phase 81.10.b adds per-agent skill rebuild.
/// - `merge_plugin_contributed_agents` is NOT re-run. Phase 18
///   does not support runtime agent removal; re-merging would
///   leave stale config on running agents. Operator must restart
///   the daemon to apply agent contribution changes.
/// - `run_plugin_init_loop` is NOT re-run. Init handles map is
///   empty today; when the manifest-driven factory ships
///   (81.7.b / 81.12), its hook slice augments this one.
pub async fn register_plugin_registry_reload_hook(
    coord: Arc<ConfigReloadCoordinator>,
    registry: Arc<NexoPluginRegistry>,
    discovery_cfg: PluginDiscoveryConfig,
    current_version: semver::Version,
) {
    coord
        .register_post_hook(Box::new(move || {
            // 1. Re-discover.
            let discovered = discover(&discovery_cfg, &current_version);

            // 2. Snapshot the previous report for delta logging.
            let prev = registry.snapshot();
            let prev_loaded = prev.last_report.loaded_ids.len();
            let prev_invalid = prev.last_report.invalid;

            let new_loaded = discovered.last_report.loaded_ids.len();
            let new_invalid = discovered.last_report.invalid;

            // 3. Build a fresh snapshot. `skill_roots` stays empty
            //    in 81.10 — see the doc-comment above.
            let new_snap = Arc::new(NexoPluginRegistrySnapshot {
                plugins: discovered.plugins.clone(),
                last_report: discovered.last_report.clone(),
                skill_roots: std::collections::BTreeMap::new(),
            });

            // 4. Atomic swap.
            registry.swap(new_snap);

            tracing::info!(
                target: "plugins.discovery",
                prev_loaded,
                new_loaded,
                delta_loaded = (new_loaded as i64) - (prev_loaded as i64),
                prev_invalid,
                new_invalid,
                delta_invalid = (new_invalid as i64) - (prev_invalid as i64),
                "post-reload plugin registry swap complete"
            );
        }))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use semver::Version;
    use tokio_util::sync::CancellationToken;

    use crate::config_reload::ConfigReloadCoordinator;
    use nexo_llm::LlmRegistry;

    fn fresh_coord() -> Arc<ConfigReloadCoordinator> {
        let tmp = tempfile::tempdir().unwrap();
        Arc::new(ConfigReloadCoordinator::new(
            tmp.path().to_path_buf(),
            Arc::new(LlmRegistry::new()),
            CancellationToken::new(),
        ))
    }

    fn write_plugin(root: &std::path::Path, plugin_id: &str) {
        std::fs::create_dir_all(root).unwrap();
        let manifest = format!(
            "[plugin]\n\
             id = \"{plugin_id}\"\n\
             version = \"0.1.0\"\n\
             name = \"{plugin_id}\"\n\
             description = \"hot-reload fixture\"\n\
             min_nexo_version = \">=0.0.1\"\n",
        );
        std::fs::write(root.join("nexo-plugin.toml"), manifest).unwrap();
    }

    #[tokio::test]
    async fn register_plugin_registry_reload_hook_pushes_one_post_hook() {
        let coord = fresh_coord();
        let registry = NexoPluginRegistry::empty();
        assert_eq!(coord.post_hooks_len_for_test().await, 0);
        register_plugin_registry_reload_hook(
            Arc::clone(&coord),
            registry,
            PluginDiscoveryConfig::default(),
            Version::new(0, 1, 0),
        )
        .await;
        assert_eq!(coord.post_hooks_len_for_test().await, 1);
    }

    #[tokio::test]
    async fn hook_replaces_snapshot_when_discover_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(&tmp.path().join("alpha"), "alpha");

        let coord = fresh_coord();
        let registry = NexoPluginRegistry::empty();
        // Sanity: empty before fire.
        assert!(registry.snapshot().last_report.loaded_ids.is_empty());

        let cfg = PluginDiscoveryConfig {
            search_paths: vec![tmp.path().to_path_buf()],
            ..Default::default()
        };
        register_plugin_registry_reload_hook(
            Arc::clone(&coord),
            Arc::clone(&registry),
            cfg,
            Version::new(0, 1, 0),
        )
        .await;

        coord.fire_post_hooks_for_test().await;

        let snap = registry.snapshot();
        assert_eq!(snap.last_report.loaded_ids, vec!["alpha".to_string()]);
        assert!(snap.skill_roots.is_empty());
    }

    #[tokio::test]
    async fn hook_swallows_discover_failure_does_not_panic() {
        let coord = fresh_coord();
        let registry = NexoPluginRegistry::empty();
        let cfg = PluginDiscoveryConfig {
            search_paths: vec![PathBuf::from(
                "/this/path/definitely/does/not/exist/__nexo_test__",
            )],
            ..Default::default()
        };
        register_plugin_registry_reload_hook(
            Arc::clone(&coord),
            Arc::clone(&registry),
            cfg,
            Version::new(0, 1, 0),
        )
        .await;
        // Must not panic.
        coord.fire_post_hooks_for_test().await;
        // Snapshot still valid; loaded empty; diagnostic recorded.
        let snap = registry.snapshot();
        assert!(snap.last_report.loaded_ids.is_empty());
        assert!(!snap.last_report.diagnostics.is_empty());
    }
}

