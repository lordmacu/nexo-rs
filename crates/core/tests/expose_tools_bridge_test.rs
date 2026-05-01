//! Phase 76.16 — integration tests for the `expose_tools` whitelist.
//!
//! Verifies that `ToolRegistryBridge` surfaces exactly the tools
//! registered in `ToolRegistry` after the `expose_tools` loop in
//! `run_mcp_server` adds them. Does NOT boot a full server — tests the
//! bridge's `list_tools()` / `call_tool()` filtering directly.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use nexo_broker::AnyBroker;
use nexo_config::types::agents::{AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig};
use nexo_core::agent::{
    mcp_server_bridge::ToolRegistryBridge,
    plan_mode_tool::{EnterPlanModeTool, ExitPlanModeTool},
    todo_write_tool::TodoWriteTool,
    tool_registry::{ToolHandler, ToolRegistry},
    AgentContext,
};
use nexo_core::session::SessionManager;
use nexo_llm::ToolDef;
use nexo_mcp::{types::McpServerInfo, McpServerHandler};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn agent_cfg() -> Arc<AgentConfig> {
    Arc::new(AgentConfig {
        id: "test-expose-tools".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m0".into(),
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
        remote_triggers: Vec::new(),
        lsp: nexo_config::types::lsp::LspPolicy::default(),
        config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
        team: nexo_config::types::team::TeamPolicy::default(),
        proactive: Default::default(),
        repl: Default::default(),
        auto_dream: None,
        assistant_mode: None,
        away_summary: None,
        brief: None,
        channels: None,
        auto_approve: false,
        extract_memories: None,
            event_subscribers: Vec::new(),
    })
}

fn minimal_ctx() -> AgentContext {
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(30), 4));
    AgentContext::new("test-expose-tools", agent_cfg(), broker, sessions)
}

fn server_info() -> McpServerInfo {
    McpServerInfo {
        name: "expose-test".into(),
        version: "0.0.1".into(),
    }
}

fn tool_names(tools: &[nexo_mcp::types::McpTool]) -> HashSet<String> {
    tools.iter().map(|t| t.name.clone()).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Empty registry → empty tool list regardless of allowlist.
#[tokio::test]
async fn empty_registry_returns_no_tools() {
    let reg = Arc::new(ToolRegistry::new());
    let bridge = ToolRegistryBridge::new(server_info(), reg, minimal_ctx(), None, false);
    let tools = bridge.list_tools().await.expect("list_tools");
    assert!(tools.is_empty(), "expected no tools, got: {tools:?}");
}

/// Two Phase-79 tools registered, allowlist = None → both visible.
#[tokio::test]
async fn no_allowlist_exposes_all_registered_tools() {
    let reg = Arc::new(ToolRegistry::new());
    reg.register(EnterPlanModeTool::tool_def(), EnterPlanModeTool);
    reg.register(TodoWriteTool::tool_def(), TodoWriteTool);

    let bridge = ToolRegistryBridge::new(server_info(), reg, minimal_ctx(), None, false);
    let tools = bridge.list_tools().await.expect("list_tools");
    let names = tool_names(&tools);

    assert!(
        names.contains("EnterPlanMode"),
        "EnterPlanMode missing: {names:?}"
    );
    assert!(names.contains("TodoWrite"), "TodoWrite missing: {names:?}");
    assert_eq!(names.len(), 2, "unexpected extra tools: {names:?}");
}

/// Allowlist filters to exactly the listed names.
#[tokio::test]
async fn allowlist_filters_to_listed_names() {
    let reg = Arc::new(ToolRegistry::new());
    reg.register(EnterPlanModeTool::tool_def(), EnterPlanModeTool);
    reg.register(ExitPlanModeTool::tool_def(), ExitPlanModeTool);
    reg.register(TodoWriteTool::tool_def(), TodoWriteTool);

    let allowlist: HashSet<String> = ["EnterPlanMode".into()].into();
    let bridge = ToolRegistryBridge::new(server_info(), reg, minimal_ctx(), Some(allowlist), false);
    let tools = bridge.list_tools().await.expect("list_tools");
    let names = tool_names(&tools);

    assert_eq!(
        names,
        HashSet::from(["EnterPlanMode".to_string()]),
        "expected only EnterPlanMode, got: {names:?}"
    );
}

/// call_tool on a tool outside the allowlist returns Err.
#[tokio::test]
async fn call_tool_blocked_by_allowlist_returns_error() {
    let reg = Arc::new(ToolRegistry::new());
    reg.register(EnterPlanModeTool::tool_def(), EnterPlanModeTool);
    reg.register(TodoWriteTool::tool_def(), TodoWriteTool);

    let allowlist: HashSet<String> = ["EnterPlanMode".into()].into();
    let bridge = ToolRegistryBridge::new(server_info(), reg, minimal_ctx(), Some(allowlist), false);

    let result: Result<_, _> = bridge
        .call_tool("TodoWrite", serde_json::json!({"todos": []}))
        .await;
    assert!(
        result.is_err(),
        "expected Err for blocked tool, got Ok: {result:?}"
    );
}

/// Proxy tools (`ext_*`, `mcp_*`) are never exposed even with allowlist = None.
#[tokio::test]
async fn proxy_tools_never_exposed() {
    #[derive(Clone)]
    struct FakeProxy;

    #[async_trait]
    impl ToolHandler for FakeProxy {
        async fn call(&self, _: &AgentContext, _: serde_json::Value) -> Result<serde_json::Value> {
            Ok(serde_json::Value::Null)
        }
    }

    let proxy_def = ToolDef {
        name: "ext_fake_proxy".into(),
        description: "proxy tool".into(),
        parameters: serde_json::json!({"type": "object"}),
    };

    let reg = Arc::new(ToolRegistry::new());
    reg.register(proxy_def, FakeProxy);
    reg.register(EnterPlanModeTool::tool_def(), EnterPlanModeTool);

    let bridge = ToolRegistryBridge::new(server_info(), reg, minimal_ctx(), None, false);
    let tools = bridge.list_tools().await.expect("list_tools");
    let names = tool_names(&tools);

    assert!(
        !names.contains("ext_fake_proxy"),
        "proxy tool should be hidden: {names:?}"
    );
    assert!(
        names.contains("EnterPlanMode"),
        "EnterPlanMode should be visible: {names:?}"
    );
    assert_eq!(
        names.len(),
        1,
        "only EnterPlanMode should appear: {names:?}"
    );
}
