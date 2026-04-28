//! Phase 79.M — end-to-end smoke for the MCP server pipeline.
//!
//! Exercises:
//!  1. `boot_exposable` constructs handlers for the requested
//!     `EXPOSABLE_TOOLS` entries.
//!  2. Each `BootResult::Registered` lands in `ToolRegistry` via
//!     `register_arc`.
//!  3. `ToolRegistryBridge` (the actual `McpServerHandler` impl
//!     used by both stdio and HTTP transports) lists the booted
//!     tools and routes `call_tool` to the same `ToolHandler`
//!     that `nexo run` uses — confirming wire-protocol parity.
//!
//! Stops short of spawning the stdio transport (covered by
//! `crates/mcp/tests/mock_server_test.rs` + `claude_2_1_conformance.rs`).
//! This test owns the integration path between the catalog, the
//! tool registry, and the MCP server handler.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_broker::AnyBroker;
use nexo_config::types::agents::{
    AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
    OutboundAllowlistConfig, WorkspaceGitConfig,
};
use nexo_config::types::mcp_exposable::EXPOSABLE_TOOLS;
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::mcp_server_bridge::{
    boot_exposable, BootResult, McpServerBootContext, ToolRegistryBridge,
};
use nexo_core::agent::tool_registry::ToolRegistry;
use nexo_core::config_changes_store::{ConfigChangeRow, ConfigChangesError, ConfigChangesStore};
use nexo_core::cron_schedule::{CronEntry, CronStore, CronStoreError};
use nexo_core::session::SessionManager;
use nexo_mcp::types::{McpServerInfo, McpToolResult};
use nexo_mcp::McpServerHandler;
use uuid::{uuid, Uuid};

fn agent_cfg() -> AgentConfig {
    AgentConfig {
        id: "smoke".into(),
        model: ModelConfig {
            provider: "x".into(),
            model: "y".into(),
        },
        plugins: Vec::new(),
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: String::new(),
        workspace: String::new(),
        skills: Vec::new(),
        skills_dir: "./skills".into(),
        skill_overrides: Default::default(),
        transcripts_dir: String::new(),
        dreaming: DreamingYamlConfig::default(),
        workspace_git: WorkspaceGitConfig::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: Vec::new(),
        inbound_bindings: Vec::new(),
        allowed_tools: Vec::new(),
        sender_rate_limit: None,
        allowed_delegates: Vec::new(),
        accept_delegates_from: Vec::new(),
        description: String::new(),
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        language: None,
        outbound_allowlist: OutboundAllowlistConfig::default(),
        context_optimization: None,
        dispatch_policy: Default::default(),
        plan_mode: Default::default(),
        remote_triggers: Vec::new(),
        lsp: nexo_config::types::lsp::LspPolicy::default(),
        config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
        team: nexo_config::types::team::TeamPolicy::default(),
        proactive: Default::default(),
    }
}

fn agent_ctx() -> Arc<AgentContext> {
    Arc::new(AgentContext::new(
        "smoke",
        Arc::new(agent_cfg()),
        AnyBroker::local(),
        Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 4)),
    ))
}

struct StubCron;
#[async_trait]
impl CronStore for StubCron {
    async fn insert(&self, _e: &CronEntry) -> Result<(), CronStoreError> {
        Ok(())
    }
    async fn list_by_binding(&self, _binding_id: &str) -> Result<Vec<CronEntry>, CronStoreError> {
        Ok(Vec::new())
    }
    async fn count_by_binding(&self, _binding_id: &str) -> Result<usize, CronStoreError> {
        Ok(0)
    }
    async fn delete(&self, _id: &str) -> Result<(), CronStoreError> {
        Ok(())
    }
    async fn due_at(&self, _now_unix: i64) -> Result<Vec<CronEntry>, CronStoreError> {
        Ok(Vec::new())
    }
    async fn set_paused(&self, _id: &str, _paused: bool) -> Result<(), CronStoreError> {
        Ok(())
    }
    async fn get(&self, _id: &str) -> Result<CronEntry, CronStoreError> {
        Err(CronStoreError::NotFound("stub".into()))
    }
    async fn advance_after_fire(
        &self,
        _id: &str,
        _new_next_fire_at: i64,
        _last_fired_at: i64,
    ) -> Result<(), CronStoreError> {
        Ok(())
    }
    async fn schedule_one_shot_retry(
        &self,
        _id: &str,
        _retry_next_fire_at: i64,
        _last_fired_at: i64,
    ) -> Result<u32, CronStoreError> {
        Ok(1)
    }
}

struct StubConfigChanges;
#[async_trait]
impl ConfigChangesStore for StubConfigChanges {
    async fn record(&self, _row: &ConfigChangeRow) -> Result<(), ConfigChangesError> {
        Ok(())
    }
    async fn tail(&self, _n: usize) -> Result<Vec<ConfigChangeRow>, ConfigChangesError> {
        Ok(Vec::new())
    }
    async fn get(&self, _patch_id: &str) -> Result<Option<ConfigChangeRow>, ConfigChangesError> {
        Ok(None)
    }
}

async fn boot_ctx() -> McpServerBootContext {
    let mut bc = McpServerBootContext::builder("smoke", AnyBroker::local(), agent_ctx())
        .cron_store(Arc::new(StubCron))
        .config_changes_store(Arc::new(StubConfigChanges))
        .build();
    bc.long_term_memory = Some(Arc::new(
        nexo_memory::LongTermMemory::open(":memory:")
            .await
            .expect("memory open"),
    ));
    // web_fetch is handle-free; web_search needs a router. Use the
    // DDG fallback (no api key required) so the test stays offline.
    bc.web_search_router = Some(Arc::new(nexo_web_search::WebSearchRouter::new(
        vec![Arc::new(
            nexo_web_search::providers::duckduckgo::DuckDuckGoProvider::new(12000),
        )],
        None,
    )));
    bc
}

/// Boots the requested tool names, registers them, wraps in the
/// bridge, and returns the bridge.
async fn build_bridge(names: &[&str]) -> ToolRegistryBridge {
    let bc = boot_ctx().await;
    let registry = Arc::new(ToolRegistry::new());
    for entry in EXPOSABLE_TOOLS {
        if !names.contains(&entry.name) {
            continue;
        }
        match boot_exposable(entry.name, &bc) {
            BootResult::Registered(def, handler) => {
                registry.register_arc(def, handler);
            }
            other => panic!(
                "expected {} to register, got {}",
                entry.name,
                other.skip_reason()
            ),
        }
    }
    ToolRegistryBridge::new(
        McpServerInfo {
            name: "smoke".into(),
            version: "0.0.1".into(),
        },
        registry,
        (*agent_ctx()).clone(),
        None,
        false,
    )
}

fn tool_result_json(res: &McpToolResult) -> serde_json::Value {
    if let Some(v) = res.structured_content.clone() {
        return v;
    }
    let first = res.content.first().expect("at least one content block");
    let text = match first {
        nexo_mcp::types::McpContent::Text { text } => text,
        other => panic!("expected text content, got {other:?}"),
    };
    serde_json::from_str(text).expect("json parse")
}

// ---------------------------------------------------------------------------
// Wire-protocol roundtrip tests
// ---------------------------------------------------------------------------

/// Boot 4 tools → list_tools returns the same set with valid schemas.
#[tokio::test]
async fn list_tools_round_trip() {
    let bridge = build_bridge(&[
        "EnterPlanMode",
        "TodoWrite",
        "cron_list",
        "config_changes_tail",
    ])
    .await;
    let tools = bridge.list_tools().await.expect("list_tools");
    let names: std::collections::HashSet<_> = tools.iter().map(|t| t.name.clone()).collect();
    assert_eq!(names.len(), 4, "wanted 4 tools, got {names:?}");
    for required in [
        "EnterPlanMode",
        "TodoWrite",
        "cron_list",
        "config_changes_tail",
    ] {
        assert!(names.contains(required), "missing {required}: {names:?}");
    }
    // Every tool must have a valid object-typed schema (MCP wire
    // contract — Claude Desktop's validator refuses anything else).
    for tool in &tools {
        assert!(
            tool.input_schema.is_object(),
            "{} schema not object",
            tool.name
        );
        let obj = tool.input_schema.as_object().unwrap();
        assert_eq!(
            obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "{} schema missing top-level type=object",
            tool.name
        );
    }
}

/// call_tool on `cron_list` (with empty args) round-trips through
/// the StubCron — verifies the same `ToolHandler` body that `nexo
/// run` invokes is invoked here, with an external-args `Value`.
#[tokio::test]
async fn call_tool_cron_list_round_trip() {
    let bridge = build_bridge(&["cron_list"]).await;
    let res: McpToolResult = bridge
        .call_tool("cron_list", serde_json::json!({}))
        .await
        .expect("call_tool");
    assert!(!res.is_error, "cron_list returned is_error=true: {res:?}");
    // The first content block is the JSON serialised tool result.
    let first = res.content.first().expect("at least one content block");
    let text = match first {
        nexo_mcp::types::McpContent::Text { text } => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&text).expect("json parse");
    // CronListTool returns either {entries: []} or empty when no
    // binding context — at minimum a JSON value, not a panic.
    assert!(
        v.is_object() || v.is_array() || v.is_null(),
        "unexpected shape: {v}"
    );
}

/// call_tool on `TodoWrite` with a valid payload returns ok.
#[tokio::test]
async fn call_tool_todo_write_round_trip() {
    let bridge = build_bridge(&["TodoWrite"]).await;
    let res = bridge
        .call_tool(
            "TodoWrite",
            serde_json::json!({
                "todos": [
                    { "content": "smoke test todo", "status": "pending", "activeForm": "testing" }
                ]
            }),
        )
        .await
        .expect("call_tool");
    assert!(!res.is_error, "TodoWrite returned is_error=true: {res:?}");
}

/// Calling an unknown tool returns Err — protocol shape.
#[tokio::test]
async fn call_tool_unknown_returns_err() {
    let bridge = build_bridge(&["EnterPlanMode"]).await;
    let r = bridge
        .call_tool("does_not_exist", serde_json::json!({}))
        .await;
    assert!(r.is_err(), "unknown tool should error, got: {r:?}");
}

/// Booted tools all carry the same name in `ToolDef.name` as the
/// catalog entry — wire-protocol identity check (the model in
/// Claude Desktop will dispatch by this exact string).
#[tokio::test]
async fn tool_def_names_match_catalog_entries() {
    let bridge = build_bridge(&[
        "ToolSearch",
        "SyntheticOutput",
        "NotebookEdit",
        "ExitPlanMode",
        "cron_create",
        "web_fetch",
    ])
    .await;
    let tools = bridge.list_tools().await.expect("list_tools");
    let names: std::collections::HashSet<_> = tools.iter().map(|t| t.name.clone()).collect();
    for n in [
        "ToolSearch",
        "SyntheticOutput",
        "NotebookEdit",
        "ExitPlanMode",
        "cron_create",
        "web_fetch",
    ] {
        assert!(names.contains(n), "{n} missing from list_tools: {names:?}");
    }
}

/// list_tools + call_tool exercise the same `McpServerHandler` API
/// that `crates/mcp/src/server/dispatch.rs` invokes for incoming
/// `tools/list` and `tools/call` JSON-RPC requests. This proves
/// the path from external MCP wire → `Dispatcher::handle_request`
/// → `handler.call_tool(name, args)` reaches our actual tool
/// implementations.
#[tokio::test]
async fn server_handler_trait_route_compiles_and_runs() {
    let bridge = build_bridge(&["EnterPlanMode"]).await;
    // Up-cast to the trait object so we exercise the same vtable
    // the dispatcher uses.
    let handler: &dyn McpServerHandler = &bridge;
    let tools = handler.list_tools().await.expect("list_tools via trait");
    assert!(tools.iter().any(|t| t.name == "EnterPlanMode"));
    // EnterPlanMode is the right surface to exercise the trait
    // route — but it deliberately refuses outside an interactive
    // origin context. We don't care about the body; we care that
    // the request reached the handler and produced a deterministic
    // wire-shaped response (is_error true with a content[0] text).
    let res = handler
        .call_tool("EnterPlanMode", serde_json::json!({}))
        .await
        .expect("call_tool via trait");
    assert!(
        !res.content.is_empty(),
        "EnterPlanMode produced no content: {res:?}"
    );
}

/// Durable follow-up flow over MCP bridge:
/// start_followup -> check_followup -> cancel_followup -> check_followup.
#[tokio::test]
async fn followup_flow_round_trip_over_mcp_bridge() {
    const EMAIL_THREAD_NS: Uuid = uuid!("c1c0a700-48e6-5000-a000-000000000000");
    let thread_root_id = "<root-123@example.com>";
    let expected_session_id = Uuid::new_v5(&EMAIL_THREAD_NS, thread_root_id.as_bytes()).to_string();

    let bridge = build_bridge(&["start_followup", "check_followup", "cancel_followup"]).await;

    let started = bridge
        .call_tool(
            "start_followup",
            serde_json::json!({
                "thread_root_id": thread_root_id,
                "instance": "sales",
                "recipient": "client@example.com",
                "check_after": "2h",
                "max_attempts": 3
            }),
        )
        .await
        .expect("start_followup");
    assert!(!started.is_error, "start_followup failed: {started:?}");
    let started_json = tool_result_json(&started);
    let flow = started_json.get("flow").expect("flow object");
    let flow_id = flow
        .get("flow_id")
        .and_then(|v| v.as_str())
        .expect("flow_id")
        .to_string();
    assert_eq!(
        flow.get("session_id").and_then(|v| v.as_str()),
        Some(expected_session_id.as_str())
    );
    assert_eq!(
        started_json.get("created").and_then(|v| v.as_bool()),
        Some(true)
    );

    let checked = bridge
        .call_tool("check_followup", serde_json::json!({ "flow_id": flow_id }))
        .await
        .expect("check_followup");
    assert!(!checked.is_error, "check_followup failed: {checked:?}");
    let checked_json = tool_result_json(&checked);
    assert_eq!(
        checked_json.get("found").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        checked_json
            .get("flow")
            .and_then(|v| v.get("status"))
            .and_then(|v| v.as_str()),
        Some("active")
    );

    let cancelled = bridge
        .call_tool(
            "cancel_followup",
            serde_json::json!({
                "flow_id": flow_id,
                "reason": "customer replied"
            }),
        )
        .await
        .expect("cancel_followup");
    assert!(!cancelled.is_error, "cancel_followup failed: {cancelled:?}");
    let cancelled_json = tool_result_json(&cancelled);
    assert_eq!(
        cancelled_json.get("cancelled").and_then(|v| v.as_bool()),
        Some(true)
    );

    let checked_after_cancel = bridge
        .call_tool("check_followup", serde_json::json!({ "flow_id": flow_id }))
        .await
        .expect("check_followup after cancel");
    assert!(
        !checked_after_cancel.is_error,
        "check_followup after cancel failed: {checked_after_cancel:?}"
    );
    let checked_after_cancel_json = tool_result_json(&checked_after_cancel);
    assert_eq!(
        checked_after_cancel_json
            .get("flow")
            .and_then(|v| v.get("status"))
            .and_then(|v| v.as_str()),
        Some("cancelled")
    );
}
