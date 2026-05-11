//! End-to-end smoke for `RemoteHookHandler`.
//! Builds a tempdir with a `nexo-plugin.toml` declaring
//! `[plugin.extends].hooks = ["before_message"]` and a bash mock
//! that:
//!
//! 1. Replies to `initialize` with the manifest's id.
//! 2. Replies to `hook.on_hook` with `HookResponse { abort: true,
//!    reason: "blocked by mock", decision: "block" }`.
//!
//! After `wire_plugin_registry_with_runtime` lands the post-init
//! hook, the test fires the hook through `wire.hook_registry` and
//! asserts `HookOutcome::Aborted`.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use nexo_broker::{AnyBroker, LocalBroker};
use nexo_config::{AgentsConfig, PluginDiscoveryConfig};
use nexo_core::agent::hook_registry::HookOutcome;
use nexo_core::agent::nexo_plugin_registry::{
    wire_plugin_registry_with_runtime, InitOutcome, PluginFactoryRegistry, SubprocessRuntime,
};
use semver::Version;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn write_remote_hook_mock(root: &std::path::Path, plugin_id: &str, hook_name: &str) {
    std::fs::create_dir_all(root).unwrap();

    let script_path = root.join("mock-plugin.sh");
    let script = format!(
        r#"#!/bin/sh
# initialize handshake
read line
echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"{plugin_id}","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0","extends":{{"hooks":["{hook_name}"]}}}}}},"server_version":"mock-0.1.0"}}}}'
# Subsequent host requests
while read line; do
    case "$line" in
        *hook.on_hook*)
            id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
            echo '{{"jsonrpc":"2.0","id":'$id',"result":{{"abort":true,"reason":"blocked by mock","decision":"block"}}}}'
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
description = "remote-hook e2e fixture"
min_nexo_version = ">=0.0.1"

[plugin.requires]
nexo_capabilities = ["broker"]

[plugin.extends]
hooks = ["{hook_name}"]

[plugin.entrypoint]
command = "{}"
"#,
        script_path.display()
    );
    std::fs::write(root.join("nexo-plugin.toml"), manifest).unwrap();
}

#[tokio::test]
async fn hook_block_decision_round_trips_via_mock_subprocess() {
    let tmp = tempdir().unwrap();
    let plugin_root = tmp.path().join("remote-hook-e2e");
    write_remote_hook_mock(&plugin_root, "remote_hook_plugin", "before_message");

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

    // Init outcome must be Ok — the post-init hook registered
    // the remote hook handler into wire.hook_registry.
    let snap = wire.registry.snapshot();
    let outcomes = &snap.last_report.init_outcomes;
    match outcomes.get("remote_hook_plugin") {
        Some(InitOutcome::Ok { duration_ms: _ }) => {}
        other => panic!(
            "expected Ok outcome for remote_hook_plugin, got {:?}",
            other
        ),
    }

    // Fire the hook — handler must round-trip the block decision
    // within 2s.
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        wire.hook_registry
            .fire("before_message", serde_json::json!({"sender": "alice"})),
    )
    .await
    .expect("hook fires within 2s");

    match outcome {
        HookOutcome::Aborted { plugin_id, reason } => {
            assert_eq!(plugin_id, "remote_hook_plugin");
            assert_eq!(reason.as_deref(), Some("blocked by mock"));
        }
        HookOutcome::Continue => panic!("expected Aborted, got Continue"),
    }

    runtime.shutdown.cancel();
    drop(wire);
    drop(broker);
    drop(tmp);
}
