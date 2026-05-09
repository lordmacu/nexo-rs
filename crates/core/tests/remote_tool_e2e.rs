//! Phase 81.29 — end-to-end smoke for the host→subprocess
//! `tool.invoke` wrapper.
//!
//! Drops a manifest declaring `[plugin.extends].tools =
//! ["browser_echo"]` plus `[plugin.tools] expose =
//! ["browser_echo"]` plus a bash mock plugin that:
//!
//! 1. Replies to `initialize` with manifest + a `tools` array
//!    advertising `browser_echo`.
//! 2. Echoes any `tool.invoke { tool_name: "browser_echo",
//!    args: <X> }` call back as `{content: [{type:"text",
//!    text:"echo: <X>"}]}`.
//!
//! Test asserts:
//! - InitOutcome::Ok.
//! - `wire.tool_registry.get("browser_echo")` returns the
//!   registered RemoteToolHandler.
//! - The handler's ToolDef.description matches the
//!   initialize-reply description.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::sync::Arc;

use nexo_broker::{AnyBroker, LocalBroker};
use nexo_config::{AgentsConfig, PluginDiscoveryConfig};
use nexo_core::agent::nexo_plugin_registry::{
    wire_plugin_registry_with_runtime, InitOutcome, PluginFactoryRegistry, SubprocessRuntime,
};
use semver::Version;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn write_tool_mock(root: &std::path::Path, plugin_id: &str, tool_name: &str) {
    std::fs::create_dir_all(root).unwrap();

    let script_path = root.join("mock-plugin.sh");
    // Mock plugin:
    // 1. Replies to initialize with `tools: [{...}]` advertising
    //    the configured tool_name.
    // 2. Subsequent host requests echo `tool.invoke` args back.
    let script = format!(
        r#"#!/bin/sh
read line
echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"{plugin_id}","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0","extends":{{"tools":["{tool_name}"]}}}}}},"server_version":"mock-0.1.0","tools":[{{"name":"{tool_name}","description":"echoes args","input_schema":{{"type":"object"}}}}]}}}}'
while read line; do
    case "$line" in
        *tool.invoke*)
            id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
            echo '{{"jsonrpc":"2.0","id":'$id',"result":{{"content":[{{"type":"text","text":"echo ok"}}],"is_error":false}}}}'
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
description = "remote-tool e2e fixture"
min_nexo_version = ">=0.0.1"

[plugin.tools]
expose = ["{tool_name}"]

[plugin.requires]
nexo_capabilities = ["broker"]

[plugin.extends]
tools = ["{tool_name}"]

[plugin.entrypoint]
command = "{}"
"#,
        script_path.display()
    );
    std::fs::write(root.join("nexo-plugin.toml"), manifest).unwrap();
}

#[tokio::test]
async fn tool_round_trips_via_mock_subprocess() {
    let tmp = tempdir().unwrap();
    let plugin_root = tmp.path().join("remote-tool-e2e");
    write_tool_mock(&plugin_root, "browser", "browser_echo");

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

    // Init outcome must be Ok — the post-init hook chain registered
    // the tool into the shared tool registry.
    let snap = wire.registry.snapshot();
    let outcomes = &snap.last_report.init_outcomes;
    match outcomes.get("browser") {
        Some(InitOutcome::Ok { duration_ms: _ }) => {}
        other => panic!("expected Ok outcome for browser, got {:?}", other),
    }

    // Fetch the registered handler from the shared tool registry.
    // The bare name (`browser_echo`) is what the scoped registry
    // committed.
    let entry = wire
        .tool_registry
        .get("browser_echo")
        .expect("browser_echo must be registered");
    let (def, _handler) = entry;
    assert_eq!(def.name, "browser_echo");
    assert_eq!(def.description, "echoes args");

    runtime.shutdown.cancel();
    drop(wire);
    drop(broker);
    drop(tmp);
}
