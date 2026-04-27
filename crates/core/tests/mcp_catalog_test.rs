//! Phase 12.3 — integration tests for `McpToolCatalog` backed by the
//! `mock_mcp_server` binary shipped in the `nexo-mcp` crate.
//!
//! The mock is a sibling workspace bin; we locate it via
//! `CARGO_MANIFEST_DIR` and rebuild it on demand so the test is self-
//! contained (`cargo test -p nexo-core --test mcp_catalog_test` works
//! from a fresh checkout).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use nexo_broker::AnyBroker;
use nexo_config::types::agents::{AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig};
use nexo_core::agent::tool_registry::ToolRegistry;
use nexo_core::agent::{AgentContext, McpToolCatalog};
use nexo_core::session::SessionManager;
use nexo_mcp::{McpServerConfig, StdioMcpClient};

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

fn config(name: &str, mode: &str) -> McpServerConfig {
    let mut env = HashMap::new();
    env.insert("MOCK_MODE".into(), mode.into());
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

fn resolve_handler(
    registry: &ToolRegistry,
    name: &str,
) -> Option<(
    nexo_llm::ToolDef,
    Arc<dyn nexo_core::agent::tool_registry::ToolHandler>,
)> {
    registry.get(name)
}

fn agent_cfg() -> Arc<AgentConfig> {
    Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: String::new(),
        workspace: String::new(),
        skills: vec![],
        skills_dir: "./skills".into(),
        skill_overrides: Default::default(),
        transcripts_dir: String::new(),
        dreaming: Default::default(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: Vec::new(),
        inbound_bindings: Vec::new(),
        allowed_tools: Vec::new(),
        sender_rate_limit: None,
        allowed_delegates: Vec::new(),
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: Default::default(),
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        language: None,
        context_optimization: None,
        dispatch_policy: Default::default(),
        plan_mode: Default::default(),
    })
}

fn null_context() -> AgentContext {
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    AgentContext::new("test-agent", agent_cfg(), broker, sessions)
}

#[tokio::test]
async fn build_populates_entries_from_happy_server() {
    let client = StdioMcpClient::connect(config("happy", "happy"))
        .await
        .expect("connect");
    let catalog = McpToolCatalog::build(vec![Arc::new(client)]).await;
    assert_eq!(catalog.servers().len(), 1);
    assert!(catalog.servers()[0].error.is_none());
    let names: Vec<&str> = catalog
        .entries()
        .iter()
        .map(|e| e.prefixed_name.as_str())
        .collect();
    assert!(names.contains(&"mcp_happy_echo"));
    assert!(names.contains(&"mcp_happy_ping"));
}

#[tokio::test]
async fn register_into_exposes_prefixed_names_in_registry() {
    let client = StdioMcpClient::connect(config("fs", "happy"))
        .await
        .expect("connect");
    let catalog = McpToolCatalog::build(vec![Arc::new(client)]).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);
    let defs = registry.to_tool_defs();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"mcp_fs_echo"), "registered: {:?}", names);
    let def = defs.iter().find(|d| d.name == "mcp_fs_echo").unwrap();
    assert!(def.description.starts_with("[mcp:fs]"));
}

#[tokio::test]
async fn tool_handler_call_flattens_text_content() {
    let client = StdioMcpClient::connect(config("happy", "happy"))
        .await
        .expect("connect");
    let catalog = McpToolCatalog::build(vec![Arc::new(client)]).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);
    let (_, handler) = resolve_handler(&registry, "mcp_happy_echo").expect("handler");
    let ctx = null_context();
    let out = handler
        .call(&ctx, serde_json::json!({"text": "hi"}))
        .await
        .expect("call");
    // Mock's happy path returns `"echo: {args}"` as text.
    assert!(out.as_str().unwrap().contains("echo"));
}

#[tokio::test]
async fn tool_handler_is_error_returns_err() {
    let client = StdioMcpClient::connect(config("broken", "tool_error"))
        .await
        .expect("connect");
    let catalog = McpToolCatalog::build(vec![Arc::new(client)]).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);
    let (_, handler) = resolve_handler(&registry, "mcp_broken_echo").expect("handler");
    let ctx = null_context();
    let err = handler
        .call(&ctx, serde_json::json!({}))
        .await
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("mcp 'broken' tool 'echo' failed"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn tool_handler_server_error_surfaces_as_err() {
    let client = StdioMcpClient::connect(config("svcerr", "server_error"))
        .await
        .expect("connect");
    let catalog = McpToolCatalog::build(vec![Arc::new(client)]).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);
    let (_, handler) = resolve_handler(&registry, "mcp_svcerr_echo").expect("handler");
    let ctx = null_context();
    let err = handler
        .call(&ctx, serde_json::json!({}))
        .await
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(msg.contains("mcp 'svcerr' tool 'echo' error"), "got: {msg}");
    assert!(msg.contains("forbidden"), "got: {msg}");
}

#[tokio::test]
async fn failed_list_tools_tracked_in_summary_without_aborting_catalog() {
    // Connect then shut one client down so its list_tools fails but the
    // build still completes; another happy client supplies entries.
    let broken = StdioMcpClient::connect(config("dead", "happy"))
        .await
        .expect("connect");
    broken.shutdown().await; // state → Shutdown → list_tools → NotReady
    let happy = StdioMcpClient::connect(config("alive", "happy"))
        .await
        .expect("connect");

    let catalog = McpToolCatalog::build(vec![Arc::new(broken), Arc::new(happy)]).await;

    let names: Vec<String> = catalog
        .servers()
        .iter()
        .map(|s| s.server_name.clone())
        .collect();
    assert!(names.contains(&"dead".to_string()));
    assert!(names.contains(&"alive".to_string()));
    let dead = catalog
        .servers()
        .iter()
        .find(|s| s.server_name == "dead")
        .unwrap();
    let alive = catalog
        .servers()
        .iter()
        .find(|s| s.server_name == "alive")
        .unwrap();
    assert!(dead.error.is_some(), "dead should report error");
    assert_eq!(dead.tool_count, 0);
    assert!(alive.error.is_none());
    assert!(alive.tool_count >= 2);
}

#[tokio::test]
async fn collisions_skip_duplicate_prefixed_names() {
    // Two independent clients with the same server name → same prefixed
    // tool names. Second registration should be dropped.
    let a = StdioMcpClient::connect(config("dup", "happy"))
        .await
        .expect("a");
    let b = StdioMcpClient::connect(config("dup", "happy"))
        .await
        .expect("b");
    let catalog = McpToolCatalog::build(vec![Arc::new(a), Arc::new(b)]).await;
    let echo_count = catalog
        .entries()
        .iter()
        .filter(|e| e.prefixed_name == "mcp_dup_echo")
        .count();
    assert_eq!(echo_count, 1);
}

#[tokio::test]
async fn sanitization_replaces_unsafe_chars_in_server_name() {
    let client = StdioMcpClient::connect(config("fs@v1", "happy"))
        .await
        .expect("connect");
    let catalog = McpToolCatalog::build(vec![Arc::new(client)]).await;
    let names: Vec<&str> = catalog
        .entries()
        .iter()
        .map(|e| e.prefixed_name.as_str())
        .collect();
    assert!(names.iter().any(|n| n.starts_with("mcp_fs_v1_")));
    assert!(!names.iter().any(|n| n.contains('@')));
}
