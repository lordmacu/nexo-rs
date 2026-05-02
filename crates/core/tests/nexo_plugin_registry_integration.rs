//! Phase 81.5 — end-to-end integration covering on-disk discovery
//! plus `ArcSwap` snapshot semantics. Builds a real plugin tree in a
//! tempdir, runs the discover walker, swaps two snapshots, and
//! asserts the registry observes the new snapshot atomically.

use std::fs;

use semver::Version;

use nexo_core::agent::nexo_plugin_registry::{
    discover, NexoPluginRegistry, PluginDiscoveryConfig,
};

fn write_manifest(dir: &std::path::Path, plugin_id: &str, version: &str) {
    fs::create_dir_all(dir).unwrap();
    let manifest = format!(
        "[plugin]\n\
         id = \"{plugin_id}\"\n\
         version = \"{version}\"\n\
         name = \"{plugin_id}\"\n\
         description = \"integration fixture\"\n\
         min_nexo_version = \">=0.0.1\"\n",
    );
    fs::write(dir.join("nexo-plugin.toml"), manifest).unwrap();
}

#[test]
fn discover_round_trip_with_arc_swap() {
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(&tmp.path().join("alpha"), "alpha", "0.1.0");
    write_manifest(&tmp.path().join("beta"), "beta", "0.2.0");

    let cfg = PluginDiscoveryConfig {
        search_paths: vec![tmp.path().to_path_buf()],
        ..Default::default()
    };
    let current = Version::parse("0.1.0").unwrap();

    let registry = NexoPluginRegistry::empty();
    assert_eq!(registry.snapshot().plugins.len(), 0);

    // First discover — both alpha and beta land.
    let snap1 = discover(&cfg, &current);
    assert_eq!(snap1.plugins.len(), 2);
    assert_eq!(snap1.last_report.invalid, 0);
    let mut ids: Vec<&str> = snap1
        .last_report
        .loaded_ids
        .iter()
        .map(|s| s.as_str())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["alpha", "beta"]);
    registry.swap(snap1);

    // Modify the tree — disable beta via config — and re-discover.
    let cfg2 = PluginDiscoveryConfig {
        search_paths: vec![tmp.path().to_path_buf()],
        disabled: vec!["beta".into()],
        ..Default::default()
    };
    let snap2 = discover(&cfg2, &current);
    assert_eq!(snap2.plugins.len(), 1);
    assert_eq!(snap2.last_report.disabled, 1);
    assert_eq!(snap2.last_report.loaded_ids, vec!["alpha".to_string()]);

    // Atomic swap — registry observes snap2 immediately.
    registry.swap(snap2);
    let observed = registry.snapshot();
    assert_eq!(observed.plugins.len(), 1);
    assert_eq!(observed.last_report.disabled, 1);
}
