//! Phase 81.5 — `NexoPluginRegistry` — hot-reloadable snapshot of
//! validated `NexoPlugin` manifests produced by [`discover`].
//!
//! This module owns the discovery contract; the manifest schema +
//! validator live in `nexo-plugin-manifest` (Phase 81.1) and the
//! plugin lifecycle trait lives in `crate::agent::plugin_host`
//! (Phase 81.2). 81.5 only produces validated manifests + paths;
//! actual `NexoPlugin::init()` invocation is Phase 81.6's job.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;

pub mod boot;
pub mod capability_aggregator;
pub mod config;
pub mod contributes;
pub mod contributes_skills;
pub mod discovery;
pub mod doctor_render;
pub mod factory;
pub mod init_loop;
pub mod report;
pub mod subprocess;

pub use boot::{
    register_plugin_registry_reload_hook, wire_plugin_registry,
    wire_plugin_registry_with_runtime, SubprocessRuntime, WirePluginRegistryOutput,
};
pub use capability_aggregator::{
    aggregate_plugin_gates, AggregatedGate, AggregatedGateState,
    PluginCapabilityAggregation, UnmetRequirement,
};
pub use config::{resolve_search_paths, PluginDiscoveryConfig};
pub use contributes::{
    merge_plugin_contributed_agents, AgentMergeConflict, AgentMergeReport,
    MergeResolution,
};
pub use contributes_skills::{
    merge_plugin_contributed_skills, SkillConflict, SkillsMergeReport,
};
pub use discovery::discover;
pub use factory::{
    BoxError, FactoryInstantiateError, FactoryRegistrationError, PluginFactory,
    PluginFactoryRegistry,
};
pub use init_loop::{run_plugin_init_loop, run_plugin_init_loop_with_factory, InitOutcome};
pub use subprocess::{subprocess_plugin_factory, SubprocessNexoPlugin};
pub use report::{
    DiagnosticLevel, DiscoveredPlugin, DiscoveryDiagnostic,
    DiscoveryDiagnosticKind, PluginDiscoveryReport,
};

/// Hot-reloadable snapshot container. Read paths are zero-contention
/// thanks to `ArcSwap`; Phase 18 hot-reload will call
/// [`Self::swap`] with a freshly-discovered snapshot.
#[derive(Debug)]
pub struct NexoPluginRegistry {
    inner: ArcSwap<NexoPluginRegistrySnapshot>,
}

#[derive(Debug, Default)]
pub struct NexoPluginRegistrySnapshot {
    pub plugins: Vec<DiscoveredPlugin>,
    pub last_report: PluginDiscoveryReport,
    /// Phase 81.7 — runtime routing data: `plugin_id -> absolute
    /// skills root` populated by `merge_plugin_contributed_skills`.
    /// Per-agent `SkillLoader::with_plugin_roots` consumes the
    /// values at boot. Empty on snapshots that pre-date 81.7 wire.
    pub skill_roots: BTreeMap<String, PathBuf>,
}

impl NexoPluginRegistry {
    /// Empty registry suitable for tests + boot-before-discover.
    pub fn empty() -> Arc<Self> {
        Arc::new(Self {
            inner: ArcSwap::from_pointee(NexoPluginRegistrySnapshot::default()),
        })
    }

    /// Cheap clone of the current snapshot for read consumers.
    pub fn snapshot(&self) -> Arc<NexoPluginRegistrySnapshot> {
        self.inner.load_full()
    }

    /// Atomically replace the active snapshot. Phase 18 reload calls
    /// this after re-running [`discover`].
    pub fn swap(&self, snap: Arc<NexoPluginRegistrySnapshot>) {
        self.inner.store(snap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_replaces_snapshot_atomically() {
        let registry = NexoPluginRegistry::empty();
        assert_eq!(registry.snapshot().plugins.len(), 0);

        let next = Arc::new(NexoPluginRegistrySnapshot {
            plugins: Vec::new(),
            last_report: PluginDiscoveryReport {
                loaded_ids: vec!["dummy".to_string()],
                ..Default::default()
            },
            skill_roots: BTreeMap::new(),
        });
        registry.swap(next);
        let observed = registry.snapshot();
        assert_eq!(observed.last_report.loaded_ids, vec!["dummy".to_string()]);
    }
}
