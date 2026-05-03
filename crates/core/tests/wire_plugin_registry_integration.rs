//! Phase 81.9 — end-to-end pipeline: build a fixture plugin tree
//! on disk that contributes one agent + one skill, run
//! `wire_plugin_registry`, assert every fold + the snapshot's
//! `skill_roots` populate as expected, and the channel adapter
//! registry comes back empty (no producer until 81.12).

use std::fs;

use semver::Version;

use nexo_config::{AgentsConfig, PluginDiscoveryConfig};
use nexo_core::agent::nexo_plugin_registry::{
    wire_plugin_registry, InitOutcome,
};

fn write_plugin_tree(root: &std::path::Path, plugin_id: &str) {
    fs::create_dir_all(root).unwrap();
    let manifest = format!(
        "[plugin]\n\
         id = \"{plugin_id}\"\n\
         version = \"0.1.0\"\n\
         name = \"{plugin_id}\"\n\
         description = \"integration\"\n\
         min_nexo_version = \">=0.0.1\"\n\
         [plugin.agents]\n\
         contributes_dir = \"agents\"\n\
         allow_override = false\n\
         [plugin.skills]\n\
         contributes_dir = \"skills\"\n",
    );
    fs::write(root.join("nexo-plugin.toml"), manifest).unwrap();

    // Contributed agent.
    let agents_dir = root.join("agents");
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

    // Contributed skill.
    let skill_dir = root.join("skills").join("triage_skill");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: triage skill\n---\n\nbody\n",
    )
    .unwrap();
}

#[tokio::test]
async fn wire_plugin_registry_full_pipeline() {
    let tmp = tempfile::tempdir().unwrap();
    write_plugin_tree(&tmp.path().join("test-plugin"), "test_plugin");

    let cfg = PluginDiscoveryConfig {
        search_paths: vec![tmp.path().to_path_buf()],
        ..Default::default()
    };
    let mut agents = AgentsConfig { agents: Vec::new() };
    let version = Version::parse("0.1.0").unwrap();

    let wire = wire_plugin_registry(
        &mut agents,
        &cfg,
        &version,
        &[],
        &std::collections::BTreeSet::new(),
        None,
    )
    .await;

    // Contributed agent landed in the runtime config.
    assert_eq!(agents.agents.len(), 1);
    assert_eq!(agents.agents[0].id, "triager");

    // skill_roots populated with one entry.
    assert_eq!(wire.skill_roots.len(), 1);

    // Channel adapter registry empty (manifest-driven instantiation
    // is 81.12).
    assert!(wire.channel_adapter_registry.kinds().is_empty());

    let snap = wire.registry.snapshot();
    let report = &snap.last_report;
    assert_eq!(report.loaded_ids, vec!["test_plugin".to_string()]);
    assert_eq!(report.invalid, 0);
    assert_eq!(report.disabled, 0);

    // 81.6 fold populated.
    assert_eq!(report.contributed_agents_per_plugin.len(), 1);
    assert_eq!(
        report
            .contributed_agents_per_plugin
            .get("test_plugin")
            .map(|v| v.as_slice()),
        Some(&["triager".to_string()][..])
    );

    // 81.7 fold populated.
    assert_eq!(report.contributed_skills_per_plugin.len(), 1);
    assert_eq!(
        report
            .contributed_skills_per_plugin
            .get("test_plugin")
            .map(|v| v.as_slice()),
        Some(&["triage_skill".to_string()][..])
    );

    // 81.6 init_outcomes populated with NoHandle for every plugin.
    assert_eq!(report.init_outcomes.len(), 1);
    assert!(matches!(
        report.init_outcomes.get("test_plugin"),
        Some(InitOutcome::NoHandle)
    ));

    // No conflicts (single plugin).
    assert!(report.agent_merge_conflicts.is_empty());
    assert!(report.skill_conflicts.is_empty());
}
