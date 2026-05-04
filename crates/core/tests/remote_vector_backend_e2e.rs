//! Phase 81.26 — end-to-end smoke for `RemoteVectorBackend`.
//! Builds a tempdir with a `nexo-plugin.toml` declaring
//! `[plugin.extends].memory_backends = ["mock_backend"]` and a
//! bash mock that:
//!
//! 1. Replies to `initialize` with the manifest's id.
//! 2. Replies to `memory.vector_upsert` → `{count: 2}`.
//! 3. Replies to `memory.vector_search` → `{matches: [...]}`.
//! 4. Replies to `memory.vector_delete` → `{count: 1}`.
//!
//! After `wire_plugin_registry_with_runtime` lands the post-init
//! hook, the test resolves the backend via
//! `wire.vector_backend_registry.get("mock_backend")` and round-
//! trips all 3 wire methods.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use nexo_broker::{AnyBroker, LocalBroker};
use nexo_config::{AgentsConfig, PluginDiscoveryConfig};
use nexo_core::agent::nexo_plugin_registry::{
    wire_plugin_registry_with_runtime, InitOutcome, PluginFactoryRegistry, SubprocessRuntime,
};
use nexo_memory::{VectorQuery, VectorRecord};
use semver::Version;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn write_remote_vector_mock(root: &std::path::Path, plugin_id: &str, backend_name: &str) {
    std::fs::create_dir_all(root).unwrap();

    let script_path = root.join("mock-plugin.sh");
    let script = format!(
        r#"#!/bin/sh
# initialize handshake
read line
echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"{plugin_id}","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0","extends":{{"memory_backends":["{backend_name}"]}}}}}},"server_version":"mock-0.1.0"}}}}'
# Subsequent host requests
while read line; do
    case "$line" in
        *memory.vector_upsert*)
            id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
            echo '{{"jsonrpc":"2.0","id":'$id',"result":{{"count":2}}}}'
            ;;
        *memory.vector_search*)
            id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
            echo '{{"jsonrpc":"2.0","id":'$id',"result":{{"matches":[{{"id":"r1","content":"hello","score":0.97,"metadata":{{}}}}]}}}}'
            ;;
        *memory.vector_delete*)
            id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
            echo '{{"jsonrpc":"2.0","id":'$id',"result":{{"count":1}}}}'
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
description = "remote-vector-backend e2e fixture"
min_nexo_version = ">=0.0.1"

[plugin.requires]
nexo_capabilities = ["broker"]

[plugin.extends]
memory_backends = ["{backend_name}"]

[plugin.entrypoint]
command = "{}"
"#,
        script_path.display()
    );
    std::fs::write(root.join("nexo-plugin.toml"), manifest).unwrap();
}

#[tokio::test]
async fn vector_ops_round_trip_via_mock_subprocess() {
    let tmp = tempdir().unwrap();
    let plugin_root = tmp.path().join("remote-vector-e2e");
    write_remote_vector_mock(&plugin_root, "remote_vector_plugin", "mock_backend");

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
    )
    .await;

    std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

    // Init outcome must be Ok — the post-init hook registered
    // the remote vector backend into wire.vector_backend_registry.
    let snap = wire.registry.snapshot();
    let outcomes = &snap.last_report.init_outcomes;
    match outcomes.get("remote_vector_plugin") {
        Some(InitOutcome::Ok { duration_ms: _ }) => {}
        other => panic!(
            "expected Ok outcome for remote_vector_plugin, got {:?}",
            other
        ),
    }

    let backend = wire
        .vector_backend_registry
        .get("mock_backend")
        .expect("mock_backend registered");
    assert_eq!(backend.name(), "mock_backend");

    // upsert
    let upsert_ack = tokio::time::timeout(
        Duration::from_secs(2),
        backend.upsert(
            "kb",
            vec![VectorRecord {
                id: "r1".into(),
                content: "hello".into(),
                embedding: vec![0.1, 0.2],
                metadata: serde_json::Value::Null,
            }],
        ),
    )
    .await
    .expect("upsert completes within 2s")
    .expect("upsert ok");
    assert_eq!(upsert_ack.count, 2);

    // search
    let matches = tokio::time::timeout(
        Duration::from_secs(2),
        backend.search(
            "kb",
            VectorQuery {
                embedding: vec![0.1, 0.2],
                limit: 5,
                filter: None,
            },
        ),
    )
    .await
    .expect("search completes within 2s")
    .expect("search ok");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, "r1");
    assert_eq!(matches[0].content, "hello");

    // delete
    let delete_ack = tokio::time::timeout(
        Duration::from_secs(2),
        backend.delete("kb", vec!["r1".into()]),
    )
    .await
    .expect("delete completes within 2s")
    .expect("delete ok");
    assert_eq!(delete_ack.count, 1);

    runtime.shutdown.cancel();
    drop(backend);
    drop(wire);
    drop(broker);
    drop(tmp);
}
