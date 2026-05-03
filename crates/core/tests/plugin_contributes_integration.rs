//! Phase 81.6 — end-to-end pipeline:
//! discover() → merge_plugin_contributed_agents() → run_plugin_init_loop()
//! Builds a real on-disk plugin tree, verifies the agent shows up
//! in the merged config, the report sections populate, and
//! `NoHandle` outcome is recorded for the empty handles map.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use semver::Version;

use nexo_config::AgentsConfig;
use nexo_core::agent::nexo_plugin_registry::{
    discover, merge_plugin_contributed_agents, run_plugin_init_loop, InitOutcome,
    NexoPluginRegistry, PluginDiscoveryConfig,
};

fn write_manifest(dir: &std::path::Path, plugin_id: &str) {
    fs::create_dir_all(dir).unwrap();
    let manifest = format!(
        "[plugin]\n\
         id = \"{plugin_id}\"\n\
         version = \"0.1.0\"\n\
         name = \"{plugin_id}\"\n\
         description = \"integration fixture\"\n\
         min_nexo_version = \">=0.0.1\"\n\
         [plugin.agents]\n\
         contributes_dir = \"agents\"\n\
         allow_override = false\n",
    );
    fs::write(dir.join("nexo-plugin.toml"), manifest).unwrap();

    let agents_dir = dir.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    let agent_yaml = "\
agents:
  - id: triager
    model:
      provider: minimax
      model: minimax-m2.5
    system_prompt: \"you triage\"
";
    fs::write(agents_dir.join("triager.yaml"), agent_yaml).unwrap();
}

#[tokio::test]
async fn discover_merge_init_pipeline_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(&tmp.path().join("test-plugin"), "test_plugin");

    let cfg = PluginDiscoveryConfig {
        search_paths: vec![tmp.path().to_path_buf()],
        ..Default::default()
    };
    let current = Version::parse("0.1.0").unwrap();

    // Discover.
    let registry = NexoPluginRegistry::empty();
    let snap = discover(&cfg, &current);
    assert_eq!(snap.plugins.len(), 1);
    assert_eq!(snap.last_report.invalid, 0);

    // Merge.
    let mut agents = AgentsConfig { agents: Vec::new() };
    let merge_report = merge_plugin_contributed_agents(&snap, &mut agents);
    assert_eq!(agents.agents.len(), 1);
    assert_eq!(agents.agents[0].id, "triager");
    assert_eq!(
        merge_report.attribution.get("triager"),
        Some(&"test_plugin".to_string())
    );
    assert_eq!(
        merge_report
            .contributed_agents_per_plugin
            .get("test_plugin")
            .map(|v| v.as_slice()),
        Some(&["triager".to_string()][..])
    );
    assert!(merge_report.conflicts.is_empty());

    // Init loop with empty handles → NoHandle expected.
    let outcomes = run_plugin_init_loop(
        &snap,
        &BTreeMap::new(),
        |_id| -> nexo_core::agent::plugin_host::PluginInitContext<'_> {
            unreachable!("ctx_factory should not be invoked when handles is empty")
        },
    )
    .await;
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes.get("test_plugin"),
        Some(InitOutcome::NoHandle)
    ));

    // Fold into the registry's report and swap.
    let mut updated = snap.last_report.clone();
    updated.fold_agent_merge(merge_report);
    updated.fold_init_outcomes(outcomes);
    let new_snap = Arc::new(
        nexo_core::agent::nexo_plugin_registry::NexoPluginRegistrySnapshot {
            plugins: snap.plugins.clone(),
            last_report: updated,
            skill_roots: BTreeMap::new(),
        },
    );
    registry.swap(new_snap);

    let observed = registry.snapshot();
    assert_eq!(observed.last_report.contributed_agents_per_plugin.len(), 1);
    assert_eq!(observed.last_report.init_outcomes.len(), 1);
    assert!(observed.last_report.agent_merge_conflicts.is_empty());
}
