//! Phase 81.24 — end-to-end smoke for `RemoteChannelAdapter`.
//! Builds a tempdir with a `nexo-plugin.toml` declaring
//! `[plugin.extends].channels = ["mock_chan"]` and points
//! `[plugin.entrypoint] command` at a small bash mock that:
//!
//! 1. Replies to `initialize` with the manifest's id.
//! 2. Reads subsequent stdin lines and replies to:
//!    - `channel.start` / `channel.stop` → `{"ok":true}`
//!    - `channel.send_outbound` → `OutboundAck`
//!
//! After `wire_plugin_registry_with_runtime` lands the post-init
//! hook, we look up the registered adapter and call
//! `send_outbound`, asserting the round-trip ack.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use nexo_broker::{AnyBroker, LocalBroker};
use nexo_config::{AgentsConfig, PluginDiscoveryConfig};
use nexo_core::agent::channel_adapter::{ChannelAdapterRegistry, OutboundMessage};
use nexo_core::agent::nexo_plugin_registry::{
    wire_plugin_registry_with_runtime, InitOutcome, PluginFactoryRegistry, SubprocessRuntime,
};
use semver::Version;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn write_remote_channel_mock(root: &std::path::Path, plugin_id: &str, kind: &str) {
    std::fs::create_dir_all(root).unwrap();

    let script_path = root.join("mock-plugin.sh");
    let script = format!(
        r#"#!/bin/sh
# initialize handshake
read line
echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"{plugin_id}","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0","extends":{{"channels":["{kind}"]}}}}}},"server_version":"mock-0.1.0"}}}}'
# subsequent host requests: dispatch by method substring
while read line; do
    case "$line" in
        *channel.start*)
            id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
            echo '{{"jsonrpc":"2.0","id":'$id',"result":{{"ok":true}}}}'
            ;;
        *channel.stop*)
            id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
            echo '{{"jsonrpc":"2.0","id":'$id',"result":{{"ok":true}}}}'
            ;;
        *channel.send_outbound*)
            id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
            echo '{{"jsonrpc":"2.0","id":'$id',"result":{{"message_id":"echo-1","sent_at_unix":1700000000}}}}'
            ;;
    esac
done
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
description = "remote-channel e2e fixture"
min_nexo_version = ">=0.0.1"

[plugin.requires]
nexo_capabilities = ["broker"]

[plugin.extends]
channels = ["{kind}"]

[plugin.entrypoint]
command = "{}"
"#,
        script_path.display()
    );
    std::fs::write(root.join("nexo-plugin.toml"), manifest).unwrap();
}

#[tokio::test]
async fn send_outbound_round_trips_via_mock_subprocess() {
    let tmp = tempdir().unwrap();
    let plugin_root = tmp.path().join("remote-channel-e2e");
    write_remote_channel_mock(&plugin_root, "remote_chan_plugin", "mock_chan");

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
        llm_registry: Arc::new(nexo_llm::LlmRegistry::new()),
        llm_config: Arc::new(nexo_config::LlmConfig {
            providers: std::collections::HashMap::new(),
            retry: Default::default(),
            context_optimization: Default::default(),
            tenants: std::collections::HashMap::new(),
        }),
        sandbox: Arc::new(nexo_core::agent::plugin_sandbox::SandboxRunner::discover()),
    };

    std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "2000");

    let wire = wire_plugin_registry_with_runtime(
        &mut agents,
        &cfg,
        &version,
        &[],
        &BTreeSet::new(),
        Some(&factory_registry),
        Some(&runtime),
        &[],
    )
    .await;

    std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

    // Init outcome must be Ok — the post-init hook registered the
    // remote channel adapter into wire.channel_adapter_registry.
    let snap = wire.registry.snapshot();
    let outcomes = &snap.last_report.init_outcomes;
    match outcomes.get("remote_chan_plugin") {
        Some(InitOutcome::Ok { duration_ms: _ }) => {}
        other => panic!(
            "expected Ok outcome for remote_chan_plugin, got {:?}",
            other
        ),
    }

    // The adapter is registered.
    let adapter = wire
        .channel_adapter_registry
        .get("mock_chan")
        .expect("mock_chan registered");
    assert_eq!(adapter.kind(), "mock_chan");

    // send_outbound round-trips an ack within 2s.
    let ack = tokio::time::timeout(
        Duration::from_secs(2),
        adapter.send_outbound(OutboundMessage::Text {
            to: "U123".into(),
            body: "hi".into(),
        }),
    )
    .await
    .expect("send_outbound completes within 2s")
    .expect("ack ok");
    assert_eq!(ack.message_id, "echo-1");
    assert_eq!(ack.sent_at_unix, 1700000000);

    // Cancel + drop everything so background tasks tear down.
    runtime.shutdown.cancel();
    drop(adapter);
    drop(wire);
    drop(broker);
    drop(tmp);
    let _ = ChannelAdapterRegistry::new(); // exercise import
}
