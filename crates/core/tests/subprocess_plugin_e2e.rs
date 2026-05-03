//! Phase 81.17.b — end-to-end smoke for the auto-subprocess
//! pipeline. Builds a tempdir with a `nexo-plugin.toml` that
//! points `[plugin.entrypoint] command` at a small bash mock,
//! runs `wire_plugin_registry_with_runtime`, asserts the init
//! outcome lands as `Ok` and that the broker bridge round-trips
//! a publish.
//!
//! The mock script is the same shape the in-tree subprocess
//! tests use — answers `initialize` with a manifest carrying
//! `id = "auto_e2e_plugin"`, then publishes one
//! `broker.publish` notification on `plugin.inbound.<kind>` so
//! the host bridge has something to forward through to a test
//! subscription.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use nexo_broker::{AnyBroker, BrokerHandle, LocalBroker};
use nexo_config::{AgentsConfig, PluginDiscoveryConfig};
use nexo_core::agent::nexo_plugin_registry::{
    wire_plugin_registry_with_runtime, InitOutcome, PluginFactoryRegistry,
    SubprocessRuntime,
};
use semver::Version;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

/// Drop a manifest + a bash mock script in the discovery search
/// path. The mock script:
/// 1. Reads one line from stdin (the `initialize` request).
/// 2. Echoes an `initialize` reply with the plugin id matching
///    the manifest's id, so handshake validation passes.
/// 3. Sleeps 0.3s so the host has time to wire the bridge tasks
///    and populate the OnceCell that gates `broker.publish`
///    forwarding.
/// 4. Echoes one `broker.publish` notification on
///    `plugin.inbound.auto_e2e_kind` carrying a fixed payload.
/// 5. Sleeps long enough that the test can drive its assertions
///    before the child exits.
fn write_mock_plugin(root: &std::path::Path, plugin_id: &str, channel_kind: &str) {
    std::fs::create_dir_all(root).unwrap();

    let script_path = root.join("mock-plugin.sh");
    let script = format!(
        r#"#!/bin/sh
read line
echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"{plugin_id}","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0"}}}},"server_version":"mock-0.1.0"}}}}'
sleep 0.3
echo '{{"jsonrpc":"2.0","method":"broker.publish","params":{{"topic":"plugin.inbound.{channel_kind}","event":{{"id":"00000000-0000-0000-0000-000000000001","timestamp":"2026-05-01T00:00:00Z","topic":"plugin.inbound.{channel_kind}","source":"auto_e2e","session_id":null,"payload":{{"hello":"world"}}}}}}}}'
sleep 5
"#
    );
    std::fs::write(&script_path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).unwrap();

    let manifest = format!(
        r#"[plugin]
id = "{plugin_id}"
version = "0.1.0"
name = "{plugin_id}"
description = "auto-subprocess e2e fixture"
min_nexo_version = ">=0.0.1"

[plugin.requires]
nexo_capabilities = ["broker"]

[[plugin.channels.register]]
kind = "{channel_kind}"
adapter = "MockAdapter"

[plugin.entrypoint]
command = "{}"
"#,
        script_path.display()
    );
    std::fs::write(root.join("nexo-plugin.toml"), manifest).unwrap();
}

#[tokio::test]
async fn auto_subprocess_pipeline_initializes_and_forwards_publish() {
    let tmp = tempdir().unwrap();
    let plugin_root = tmp.path().join("auto-e2e-plugin");
    write_mock_plugin(&plugin_root, "auto_e2e_plugin", "auto_e2e_kind");

    let cfg = PluginDiscoveryConfig {
        search_paths: vec![tmp.path().to_path_buf()],
        ..Default::default()
    };
    let mut agents = AgentsConfig { agents: Vec::new() };
    let version = Version::parse("0.1.0").unwrap();

    let broker = AnyBroker::Local(LocalBroker::new());
    // Subscribe BEFORE wiring so the broker is ready when the
    // mock script's publish lands. LocalBroker is Arc<DashMap>
    // internally so subscriptions across clones share state.
    let mut sub = broker
        .subscribe("plugin.inbound.auto_e2e_kind")
        .await
        .expect("subscribe");

    let factory_registry = PluginFactoryRegistry::new();
    let runtime = SubprocessRuntime {
        broker: broker.clone(),
        shutdown: CancellationToken::new(),
        config_dir: tmp.path().to_path_buf(),
        state_root: tmp.path().to_path_buf(),
        long_term_memory: None,
        llm: None,
    };

    // Cap the initialize-reply window short for the test so a
    // hung mock fails fast.
    std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "2000");

    let wire = wire_plugin_registry_with_runtime(
        &mut agents,
        &cfg,
        &version,
        &[],
        &BTreeSet::new(),
        Some(&factory_registry),
        Some(&runtime),
    )
    .await;

    std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

    // Init outcome: must be `Ok` — the auto-subprocess fallback
    // built the factory, spawned the mock, validated the manifest
    // id, and wired the broker bridge.
    let snap = wire.registry.snapshot();
    let outcomes = &snap.last_report.init_outcomes;
    match outcomes.get("auto_e2e_plugin") {
        Some(InitOutcome::Ok { duration_ms: _ }) => {}
        other => panic!(
            "expected Ok outcome for auto_e2e_plugin, got {:?}",
            other
        ),
    }

    // Broker bridge: the mock's `broker.publish` notification
    // must round-trip through the host's allowlist + reach our
    // subscription within ~2s. LocalBroker has no jitter; if this
    // ever hits 2s it means the bridge isn't forwarding.
    let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("event arrives within 2s");
    let event = event.expect("subscription delivers Some");
    assert_eq!(event.topic, "plugin.inbound.auto_e2e_kind");
    assert_eq!(event.source, "auto_e2e");
    assert_eq!(event.payload.get("hello").and_then(|v| v.as_str()), Some("world"));

    // Cancel so any background tasks the bridge spawned exit.
    runtime.shutdown.cancel();
    drop(wire);
    // Drop the broker to release LocalBroker's Arc'd subs map.
    drop(broker);
    drop(sub);
    // Hold the tempdir so the script path stays valid; tempdir
    // drops at scope end which deletes everything cleanly.
    drop(tmp);
}

#[tokio::test]
async fn manifest_without_entrypoint_keeps_no_handle_outcome() {
    // Defensive regression: a manifest WITHOUT `entrypoint.command`
    // (the in-tree-plugin shape from 81.12.a-d) must NOT
    // accidentally trigger the auto-subprocess fallback when the
    // boot wire is `Some(&factory_registry) + Some(&runtime)`. Any
    // change to the fallback predicate that breaks this invariant
    // would attempt to spawn the in-tree dormant manifests as
    // subprocesses, panic-loop the daemon at boot.
    let tmp = tempdir().unwrap();
    let plugin_root = tmp.path().join("in-tree-shape");
    std::fs::create_dir_all(&plugin_root).unwrap();
    std::fs::write(
        plugin_root.join("nexo-plugin.toml"),
        r#"[plugin]
id = "in_tree_shape"
version = "0.1.0"
name = "in_tree_shape"
description = "fixture without entrypoint"
min_nexo_version = ">=0.0.1"

[plugin.requires]
nexo_capabilities = ["broker"]
"#,
    )
    .unwrap();

    let cfg = PluginDiscoveryConfig {
        search_paths: vec![tmp.path().to_path_buf()],
        ..Default::default()
    };
    let mut agents = AgentsConfig { agents: Vec::new() };
    let version = Version::parse("0.1.0").unwrap();
    let broker = AnyBroker::Local(LocalBroker::new());
    let factory_registry = PluginFactoryRegistry::new();
    let runtime = SubprocessRuntime {
        broker: broker.clone(),
        shutdown: CancellationToken::new(),
        config_dir: tmp.path().to_path_buf(),
        state_root: tmp.path().to_path_buf(),
        long_term_memory: None,
        llm: None,
    };

    let wire = wire_plugin_registry_with_runtime(
        &mut agents,
        &cfg,
        &version,
        &[],
        &BTreeSet::new(),
        Some(&factory_registry),
        Some(&runtime),
    )
    .await;

    let snap = wire.registry.snapshot();
    assert!(matches!(
        snap.last_report.init_outcomes.get("in_tree_shape"),
        Some(InitOutcome::NoHandle)
    ));

    let _ = Arc::strong_count(&wire.registry);
}
