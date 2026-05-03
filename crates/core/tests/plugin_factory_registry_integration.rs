//! Phase 81.12.0 — verify `wire_plugin_registry(..., Some(&factory))`
//! consults the factory registry for each discovered plugin and
//! records `Failed` (factory closure errors) instead of `NoHandle`.
//! We force the factory to error so the helper short-circuits
//! BEFORE invoking `ctx_factory` — building a real
//! `PluginInitContext` is heavy and not needed to validate the
//! routing path.

use std::collections::BTreeSet;
use std::fs;

use semver::Version;

use nexo_config::{AgentsConfig, PluginDiscoveryConfig};
use nexo_core::agent::nexo_plugin_registry::{
    wire_plugin_registry, BoxError, InitOutcome, PluginFactory,
    PluginFactoryRegistry,
};

fn write_plugin_tree(root: &std::path::Path, plugin_id: &str) {
    fs::create_dir_all(root).unwrap();
    let manifest = format!(
        "[plugin]\n\
         id = \"{plugin_id}\"\n\
         version = \"0.1.0\"\n\
         name = \"{plugin_id}\"\n\
         description = \"factory integration\"\n\
         min_nexo_version = \">=0.0.1\"\n",
    );
    fs::write(root.join("nexo-plugin.toml"), manifest).unwrap();
}

#[tokio::test]
async fn factory_driven_path_records_failed_outcome_for_registered_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    write_plugin_tree(&tmp.path().join("factory_demo"), "factory_demo");

    let cfg = PluginDiscoveryConfig {
        search_paths: vec![tmp.path().to_path_buf()],
        ..Default::default()
    };
    let mut agents = AgentsConfig { agents: Vec::new() };
    let version = Version::parse("0.1.0").unwrap();

    // Register a factory that DELIBERATELY fails so we exercise
    // the factory-driven path without having to build a real
    // PluginInitContext.
    let mut registry = PluginFactoryRegistry::new();
    let factory: PluginFactory = Box::new(|_m| {
        let err: BoxError =
            Box::new(std::io::Error::other("factory deliberately failed"));
        Err(err)
    });
    registry.register("factory_demo", factory).unwrap();

    let wire = wire_plugin_registry(
        &mut agents,
        &cfg,
        &version,
        &[],
        &BTreeSet::new(),
        Some(&registry),
    )
    .await;

    let snap = wire.registry.snapshot();
    let outcome = snap
        .last_report
        .init_outcomes
        .get("factory_demo")
        .expect("factory_demo init outcome must be recorded");
    match outcome {
        InitOutcome::Failed { error } => {
            assert!(
                error.contains("factory deliberately failed"),
                "expected boxed source in error message: {error}"
            );
        }
        other => panic!(
            "factory_demo registered → must be Failed (closure errored), got {other:?}"
        ),
    }
}

#[tokio::test]
async fn factory_none_path_preserves_no_handle_default() {
    let tmp = tempfile::tempdir().unwrap();
    write_plugin_tree(&tmp.path().join("legacy_demo"), "legacy_demo");

    let cfg = PluginDiscoveryConfig {
        search_paths: vec![tmp.path().to_path_buf()],
        ..Default::default()
    };
    let mut agents = AgentsConfig { agents: Vec::new() };
    let version = Version::parse("0.1.0").unwrap();

    // None → behaves identical to pre-81.12.0: every plugin
    // records NoHandle (no factory consulted).
    let wire = wire_plugin_registry(
        &mut agents,
        &cfg,
        &version,
        &[],
        &BTreeSet::new(),
        None,
    )
    .await;

    let snap = wire.registry.snapshot();
    assert!(matches!(
        snap.last_report.init_outcomes.get("legacy_demo"),
        Some(InitOutcome::NoHandle)
    ));
}
