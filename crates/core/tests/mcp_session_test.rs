//! Phase 12.4 — `nexo-core` helpers that bridge `SessionMcpRuntime` to
//! the agent's `ToolRegistry`. Reuses the manager from `nexo-mcp` and the
//! mock server binary (rebuilt on demand).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use nexo_core::agent::tool_registry::ToolRegistry;
use nexo_core::agent::{build_session_catalog, register_session_tools};
use nexo_mcp::config::McpServerConfig;
use nexo_mcp::runtime_config::{McpRuntimeConfig, McpServerRuntimeConfig};
use nexo_mcp::McpRuntimeManager;
use uuid::Uuid;

static BUILT: OnceLock<PathBuf> = OnceLock::new();

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn mock_bin_path() -> PathBuf {
    BUILT
        .get_or_init(|| {
            let path = workspace_root().join("target/debug/mock_mcp_server");
            if !path.exists() {
                let status = Command::new(env!("CARGO"))
                    .args(["build", "--bin", "mock_mcp_server", "-p", "nexo-mcp"])
                    .status()
                    .expect("cargo build mock_mcp_server");
                assert!(status.success(), "failed to build mock_mcp_server");
            }
            assert!(path.exists(), "mock server not at {}", path.display());
            path
        })
        .clone()
}

fn server_config(name: &str) -> McpServerConfig {
    let mut env = HashMap::new();
    env.insert("MOCK_MODE".into(), "happy".into());
    McpServerConfig {
        name: name.into(),
        command: mock_bin_path().display().to_string(),
        env,
        connect_timeout: Duration::from_secs(5),
        initialize_timeout: Duration::from_millis(500),
        call_timeout: Duration::from_millis(500),
        shutdown_grace: Duration::from_millis(100),
        ..Default::default()
    }
}

fn manager_with(servers: Vec<McpServerConfig>) -> Arc<McpRuntimeManager> {
    McpRuntimeManager::new(McpRuntimeConfig {
        servers: servers
            .into_iter()
            .map(McpServerRuntimeConfig::Stdio)
            .collect(),
        session_ttl: Duration::from_secs(60),
        idle_reap_interval: Duration::from_secs(1),
        reset_level_on_unset: false,
        default_reset_level: "info".into(),
        resource_cache: Default::default(),
        resource_uri_allowlist: Vec::new(),
    })
}

#[tokio::test]
async fn build_session_catalog_lists_tools_from_all_servers() {
    let mgr = manager_with(vec![server_config("alpha"), server_config("beta")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = build_session_catalog(&rt).await;
    let names: Vec<&str> = catalog
        .entries()
        .iter()
        .map(|e| e.prefixed_name.as_str())
        .collect();
    assert!(names.contains(&"mcp_alpha_echo"));
    assert!(names.contains(&"mcp_beta_echo"));
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn register_session_tools_populates_registry() {
    let mgr = manager_with(vec![server_config("fs")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let registry = ToolRegistry::new();
    register_session_tools(&rt, &registry).await;
    let defs = registry.to_tool_defs();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"mcp_fs_echo"), "got: {names:?}");
    let def = defs.iter().find(|d| d.name == "mcp_fs_echo").unwrap();
    assert!(def.description.starts_with("[mcp:fs]"));
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn build_session_catalog_touches_runtime() {
    let mgr = manager_with(vec![server_config("alpha")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let first = rt.last_used_at();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = build_session_catalog(&rt).await;
    let after = rt.last_used_at();
    assert!(after > first, "catalog build should refresh last_used_at");
    mgr.shutdown_all().await;
}
