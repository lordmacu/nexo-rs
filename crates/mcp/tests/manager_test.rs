//! Phase 12.4 — integration tests for `McpRuntimeManager`.
//!
//! Uses the `mock_mcp_server` binary (a `[[bin]]` in this crate) so
//! `CARGO_BIN_EXE_mock_mcp_server` resolves and gives us a real child
//! process speaking JSON-RPC.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_mcp::config::McpServerConfig;
use agent_mcp::runtime_config::{McpRuntimeConfig, McpServerRuntimeConfig};
use agent_mcp::{McpRuntimeManager, RuntimeCallError};
use uuid::Uuid;

fn mock_server_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_mcp_server"))
}

fn server_config(name: &str, mode: &str) -> McpServerConfig {
    let mut env = HashMap::new();
    env.insert("MOCK_MODE".into(), mode.into());
    McpServerConfig {
        name: name.into(),
        command: mock_server_path().display().to_string(),
        env,
        connect_timeout: Duration::from_secs(5),
        initialize_timeout: Duration::from_millis(500),
        call_timeout: Duration::from_millis(500),
        shutdown_grace: Duration::from_millis(100),
        ..Default::default()
    }
}

fn runtime_config(servers: Vec<McpServerConfig>, ttl_ms: u64) -> McpRuntimeConfig {
    McpRuntimeConfig {
        servers: servers.into_iter().map(McpServerRuntimeConfig::Stdio).collect(),
        session_ttl: Duration::from_millis(ttl_ms),
        idle_reap_interval: Duration::from_millis((ttl_ms / 2).max(20)),
        reset_level_on_unset: false,
        default_reset_level: "info".into(),
        resource_cache: Default::default(),
        resource_uri_allowlist: Vec::new(),
    }
}

#[tokio::test]
async fn reuses_runtime_when_fingerprint_stable() {
    let mgr = McpRuntimeManager::new(runtime_config(
        vec![server_config("happy", "happy")],
        60_000,
    ));
    let sid = Uuid::new_v4();
    let a = mgr.get_or_create(sid).await;
    let b = mgr.get_or_create(sid).await;
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(a.clients().len(), 1);
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn recreates_runtime_on_fingerprint_change() {
    let mgr = McpRuntimeManager::new(runtime_config(
        vec![server_config("happy", "happy")],
        60_000,
    ));
    let sid = Uuid::new_v4();
    let a = mgr.get_or_create(sid).await;

    let new_cfg = runtime_config(
        vec![server_config("different_name", "happy")],
        60_000,
    );
    assert_ne!(new_cfg.fingerprint(), a.config_fingerprint());
    mgr.update_config(new_cfg).await;

    let b = mgr.get_or_create(sid).await;
    assert!(a.is_disposed(), "old runtime should be disposed");
    assert_ne!(a.config_fingerprint(), b.config_fingerprint());
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn concurrent_get_or_create_deduplicates() {
    let mgr = McpRuntimeManager::new(runtime_config(
        vec![server_config("happy", "happy")],
        60_000,
    ));
    let sid = Uuid::new_v4();

    let mgr_a = mgr.clone();
    let mgr_b = mgr.clone();
    let (a, b) = tokio::join!(
        async move { mgr_a.get_or_create(sid).await },
        async move { mgr_b.get_or_create(sid).await }
    );
    assert!(Arc::ptr_eq(&a, &b), "concurrent creates must share one runtime");
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn dispose_session_shuts_down_clients() {
    let mgr = McpRuntimeManager::new(runtime_config(
        vec![server_config("happy", "happy")],
        60_000,
    ));
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    assert_eq!(rt.clients().len(), 1);
    mgr.dispose_session(sid).await;
    assert!(rt.is_disposed());
    assert!(mgr.get(sid).is_none());
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn reap_evicts_idle_session() {
    let mgr = McpRuntimeManager::new(runtime_config(
        vec![server_config("happy", "happy")],
        60, // 60ms TTL
    ));
    let sid = Uuid::new_v4();
    let _ = mgr.get_or_create(sid).await;
    // Wait past TTL (60ms) plus at least one reap tick (30ms).
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(mgr.get(sid).is_none(), "session should have been reaped");
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn call_tool_routes_to_correct_server() {
    let mgr = McpRuntimeManager::new(runtime_config(
        vec![
            server_config("alpha", "happy"),
            server_config("beta", "happy"),
        ],
        60_000,
    ));
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    assert_eq!(rt.clients().len(), 2);

    let result = rt
        .call_tool("alpha", "echo", serde_json::json!({"msg": "hi"}))
        .await
        .expect("call ok");
    assert!(!result.is_error);
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn unknown_server_returns_runtime_error() {
    let mgr = McpRuntimeManager::new(runtime_config(
        vec![server_config("alpha", "happy")],
        60_000,
    ));
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let err = rt
        .call_tool("ghost", "echo", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeCallError::UnknownServer(_)));
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn failed_server_excluded_from_clients() {
    // Mix one good server and one with an unspawnable command.
    let good = server_config("good", "happy");
    let bad = McpServerConfig {
        name: "bad".into(),
        command: "/definitely/nonexistent/binary".into(),
        connect_timeout: Duration::from_millis(200),
        initialize_timeout: Duration::from_millis(200),
        call_timeout: Duration::from_millis(200),
        shutdown_grace: Duration::from_millis(50),
        ..Default::default()
    };
    let mgr = McpRuntimeManager::new(runtime_config(vec![good, bad], 60_000));
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let names: Vec<String> = rt.clients().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, vec!["good"]);
    mgr.shutdown_all().await;
}
