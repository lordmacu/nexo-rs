//! End-to-end smoke for `RemoteLlmClient`. Builds
//! a tempdir with a `nexo-plugin.toml` declaring
//! `[plugin.extends].llm_providers = ["mock_llm"]` and a bash
//! mock that:
//!
//! 1. Replies to `initialize` with the manifest's id.
//! 2. Replies to `llm.chat` requests with a synthetic
//!    `WireChatResponse`.
//!
//! After `wire_plugin_registry_with_runtime` lands the post-init
//! hook, the test resolves the provider via
//! `runtime.llm_registry.build(...)` and calls `chat()`,
//! asserting the round-trip.

#![cfg(unix)]

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use nexo_broker::{AnyBroker, LocalBroker};
use nexo_config::types::agents::ModelConfig;
use nexo_config::types::llm::{LlmProviderConfig, RetryConfig};
use nexo_config::{AgentsConfig, LlmConfig, PluginDiscoveryConfig};
use nexo_core::agent::nexo_plugin_registry::{
    wire_plugin_registry_with_runtime, InitOutcome, PluginFactoryRegistry, SubprocessRuntime,
};
use nexo_llm::types::{ChatMessage, ChatRequest, ChatRole};
use semver::Version;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn write_remote_llm_mock(root: &std::path::Path, plugin_id: &str, provider: &str) {
    std::fs::create_dir_all(root).unwrap();

    let script_path = root.join("mock-plugin.sh");
    let script = format!(
        r#"#!/bin/sh
# initialize handshake
read line
echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"{plugin_id}","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0","extends":{{"llm_providers":["{provider}"]}}}}}},"server_version":"mock-0.1.0"}}}}'
# Subsequent host requests: dispatch by method substring.
while read line; do
    case "$line" in
        *llm.chat*)
            id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
            echo '{{"jsonrpc":"2.0","id":'$id',"result":{{"content":{{"type":"text","text":"hello from mock"}},"usage":{{"prompt_tokens":4,"completion_tokens":3}},"finish_reason":{{"kind":"stop"}}}}}}'
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
description = "remote-llm e2e fixture"
min_nexo_version = ">=0.0.1"

[plugin.requires]
nexo_capabilities = ["broker"]

[plugin.extends]
llm_providers = ["{provider}"]

[plugin.entrypoint]
command = "{}"
"#,
        script_path.display()
    );
    std::fs::write(root.join("nexo-plugin.toml"), manifest).unwrap();
}

#[tokio::test]
async fn llm_chat_round_trips_via_mock_subprocess() {
    let tmp = tempdir().unwrap();
    let plugin_root = tmp.path().join("remote-llm-e2e");
    write_remote_llm_mock(&plugin_root, "remote_llm_plugin", "mock_llm");

    let cfg = PluginDiscoveryConfig {
        search_paths: vec![tmp.path().to_path_buf()],
        ..Default::default()
    };
    let mut agents = AgentsConfig { agents: Vec::new() };
    let version = Version::parse("0.1.0").unwrap();

    let broker = AnyBroker::Local(LocalBroker::new());
    let factory_registry = PluginFactoryRegistry::new();
    let llm_registry = Arc::new(nexo_llm::LlmRegistry::new());
    let runtime = SubprocessRuntime {
        broker: broker.clone(),
        shutdown: CancellationToken::new(),
        config_dir: tmp.path().to_path_buf(),
        state_root: tmp.path().to_path_buf(),
        long_term_memory: None,
        llm_registry: llm_registry.clone(),
        llm_config: Arc::new(LlmConfig {
            providers: HashMap::new(),
            retry: Default::default(),
            context_optimization: Default::default(),
            tenants: HashMap::new(),
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
    // the remote LLM provider into runtime.llm_registry.
    let snap = wire.registry.snapshot();
    let outcomes = &snap.last_report.init_outcomes;
    match outcomes.get("remote_llm_plugin") {
        Some(InitOutcome::Ok { duration_ms: _ }) => {}
        other => panic!("expected Ok outcome for remote_llm_plugin, got {:?}", other),
    }

    // Resolve the provider via the registry → RemoteLlmClient.
    // We need a synthetic LlmConfig.providers entry so
    // build_for_tenant doesn't error on missing config (the
    // remote factory ignores provider_cfg in its build()).
    let mut providers = HashMap::new();
    providers.insert(
        "mock_llm".to_string(),
        LlmProviderConfig {
            api_key: String::new(),
            base_url: String::new(),
            group_id: None,
            rate_limit: Default::default(),
            auth: None,
            api_flavor: None,
            embedding_model: None,
            safety_settings: Default::default(),
            factory_type: None,
            api_key_secret_id: None,
        },
    );
    let llm_cfg = LlmConfig {
        providers,
        retry: RetryConfig::default(),
        context_optimization: Default::default(),
        tenants: HashMap::new(),
    };
    let agent_model = ModelConfig {
        provider: "mock_llm".into(),
        model: "any".into(),
    };

    let client = llm_registry
        .build(&llm_cfg, &agent_model)
        .expect("build remote client");
    assert_eq!(client.provider(), "mock_llm");
    assert_eq!(client.model_id(), "any");

    // chat() round-trips the synthetic response within 2s.
    let req = ChatRequest::new(
        "any",
        vec![ChatMessage {
            role: ChatRole::User,
            content: "hi".into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }],
    );
    let resp = tokio::time::timeout(Duration::from_secs(2), client.chat(req))
        .await
        .expect("chat completes within 2s")
        .expect("chat ok");
    match resp.content {
        nexo_llm::types::ResponseContent::Text(t) => assert_eq!(t, "hello from mock"),
        other => panic!("expected Text, got {other:?}"),
    }
    assert_eq!(resp.usage.prompt_tokens, 4);
    assert_eq!(resp.usage.completion_tokens, 3);

    runtime.shutdown.cancel();
    drop(client);
    drop(wire);
    drop(broker);
    drop(tmp);
}
