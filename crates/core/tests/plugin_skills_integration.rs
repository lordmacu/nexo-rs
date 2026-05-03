//! Phase 81.7 — end-to-end plugin skill discovery + load via
//! `SkillLoader::with_plugin_roots`. Builds two on-disk plugin
//! trees, runs `merge_plugin_contributed_skills`, threads the
//! resulting roots into a `SkillLoader`, asserts both contributed
//! skills load.

use std::collections::BTreeMap;
use std::fs;

use nexo_core::agent::nexo_plugin_registry::{
    merge_plugin_contributed_skills, DiscoveredPlugin,
    NexoPluginRegistrySnapshot, PluginDiscoveryReport,
};
use nexo_core::agent::skills::SkillLoader;
use nexo_plugin_manifest::PluginManifest;

fn write_manifest(dir: &std::path::Path, plugin_id: &str) {
    fs::create_dir_all(dir).unwrap();
    let manifest = format!(
        "[plugin]\n\
         id = \"{plugin_id}\"\n\
         version = \"0.1.0\"\n\
         name = \"{plugin_id}\"\n\
         description = \"integration\"\n\
         min_nexo_version = \">=0.0.1\"\n\
         [plugin.skills]\n\
         contributes_dir = \"skills\"\n",
    );
    fs::write(dir.join("nexo-plugin.toml"), manifest).unwrap();
    fs::create_dir_all(dir.join("skills")).unwrap();
}

fn write_skill(plugin_root: &std::path::Path, name: &str, body: &str) {
    let skill_dir = plugin_root.join("skills").join(name);
    fs::create_dir_all(&skill_dir).unwrap();
    let content =
        format!("---\ndescription: integration test skill\n---\n\n{body}\n");
    fs::write(skill_dir.join("SKILL.md"), content).unwrap();
}

fn discovered(dir: &std::path::Path) -> DiscoveredPlugin {
    let raw = fs::read_to_string(dir.join("nexo-plugin.toml")).unwrap();
    let manifest: PluginManifest = toml::from_str(&raw).unwrap();
    DiscoveredPlugin {
        manifest,
        root_dir: dir.to_path_buf(),
        manifest_path: dir.join("nexo-plugin.toml"),
    }
}

#[tokio::test]
async fn merge_then_skill_loader_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_a_root = tmp.path().join("plugin_a");
    let plugin_b_root = tmp.path().join("plugin_b");
    write_manifest(&plugin_a_root, "alpha");
    write_manifest(&plugin_b_root, "beta");
    write_skill(&plugin_a_root, "alpha_skill", "alpha body content");
    write_skill(&plugin_b_root, "beta_skill", "beta body content");

    let snap = NexoPluginRegistrySnapshot {
        plugins: vec![discovered(&plugin_a_root), discovered(&plugin_b_root)],
        last_report: PluginDiscoveryReport::default(),
        skill_roots: BTreeMap::new(),
    };

    let report = merge_plugin_contributed_skills(&snap);
    assert_eq!(report.skill_roots.len(), 2);
    assert_eq!(report.attribution.len(), 2);
    assert!(report.conflicts.is_empty());

    // Operator's skills_dir is empty (just an empty tempdir) so
    // every skill resolves via plugin_roots.
    let operator_root = tempfile::tempdir().unwrap();
    let plugin_roots: Vec<_> = report.skill_roots.values().cloned().collect();
    let loader =
        SkillLoader::new(operator_root.path()).with_plugin_roots(plugin_roots);

    let loaded = loader
        .load_many(&[
            "alpha_skill".to_string(),
            "beta_skill".to_string(),
        ])
        .await;
    assert_eq!(loaded.len(), 2);
    let names: Vec<&str> = loaded.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"alpha_skill"));
    assert!(names.contains(&"beta_skill"));
}
