//! Phase 12.5 — integration: McpToolCatalog registers resource meta-tools
//! only for servers that advertise the capability, and the registered
//! handlers round-trip against the mock MCP server.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use agent_broker::AnyBroker;
use agent_config::types::agents::{AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig};
use agent_core::agent::tool_registry::ToolRegistry;
use agent_core::agent::{AgentContext, McpToolCatalog};
use agent_core::session::SessionManager;
use agent_mcp::config::McpServerConfig;
use agent_mcp::runtime_config::{McpRuntimeConfig, McpServerRuntimeConfig};
use agent_mcp::McpRuntimeManager;
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
                    .args(["build", "--bin", "mock_mcp_server", "-p", "agent-mcp"])
                    .status()
                    .expect("cargo build mock_mcp_server");
                assert!(status.success(), "failed to build mock_mcp_server");
            }
            assert!(path.exists());
            path
        })
        .clone()
}

fn server_config(name: &str, mode: &str) -> McpServerConfig {
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

fn manager_with(servers: Vec<McpServerConfig>) -> Arc<McpRuntimeManager> {
    manager_with_cache(servers, Default::default())
}

fn manager_with_cache(
    servers: Vec<McpServerConfig>,
    cache: agent_mcp::ResourceCacheConfig,
) -> Arc<McpRuntimeManager> {
    manager_with_cache_and_allowlist(servers, cache, Vec::new())
}

fn manager_with_cache_and_allowlist(
    servers: Vec<McpServerConfig>,
    cache: agent_mcp::ResourceCacheConfig,
    allowlist: Vec<String>,
) -> Arc<McpRuntimeManager> {
    McpRuntimeManager::new(McpRuntimeConfig {
        servers: servers
            .into_iter()
            .map(McpServerRuntimeConfig::Stdio)
            .collect(),
        session_ttl: Duration::from_secs(60),
        idle_reap_interval: Duration::from_secs(1),
        reset_level_on_unset: false,
        default_reset_level: "info".into(),
        resource_cache: cache,
        resource_uri_allowlist: allowlist,
    })
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
            language: None,
    })
}

fn null_context() -> AgentContext {
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    AgentContext::new("test-agent", agent_cfg(), broker, sessions)
}

#[tokio::test]
async fn resource_meta_tools_registered_for_capable_server() {
    let mgr = manager_with(vec![server_config("fs", "resources")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = McpToolCatalog::build(rt.clients().into_iter().map(|(_, c)| c).collect()).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);
    let names: Vec<String> = registry
        .to_tool_defs()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(
        names.iter().any(|n| n == "mcp_fs_list_resources"),
        "got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "mcp_fs_read_resource"),
        "got {names:?}"
    );
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn resource_meta_tools_skipped_when_capability_absent() {
    let mgr = manager_with(vec![server_config("fs", "happy")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = McpToolCatalog::build(rt.clients().into_iter().map(|(_, c)| c).collect()).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);
    let names: Vec<String> = registry
        .to_tool_defs()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(!names.iter().any(|n| n.contains("_list_resources")));
    assert!(!names.iter().any(|n| n.contains("_read_resource")));
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn list_resources_handler_returns_array() {
    let mgr = manager_with(vec![server_config("fs", "resources")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = McpToolCatalog::build(rt.clients().into_iter().map(|(_, c)| c).collect()).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);

    let (_, handler) = registry.get("mcp_fs_list_resources").expect("registered");
    let ctx = null_context();
    let out = handler
        .call(&ctx, serde_json::json!({}))
        .await
        .expect("call");
    let arr = out.as_array().expect("array");
    assert!(arr.len() >= 2);
    assert!(arr.iter().any(|v| v["uri"] == "file:///readme"));
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn register_into_does_not_overwrite_native() {
    use agent_core::agent::tool_registry::ToolHandler;
    use agent_llm::ToolDef;
    use async_trait::async_trait;
    use serde_json::Value;

    struct NativeEcho;
    #[async_trait]
    impl ToolHandler for NativeEcho {
        async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            Ok(Value::String("native".into()))
        }
    }

    let mgr = manager_with(vec![server_config("happy", "happy")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = McpToolCatalog::build(rt.clients().into_iter().map(|(_, c)| c).collect()).await;

    // Pre-register a native handler under the name the MCP catalog will
    // produce for server "happy" + tool "echo".
    let registry = ToolRegistry::new();
    registry.register(
        ToolDef {
            name: "mcp_happy_echo".into(),
            description: "native".into(),
            parameters: serde_json::json!({"type":"object" }),
        },
        NativeEcho,
    );

    catalog.register_into(&registry);

    // Native handler preserved; ping registered fresh.
    let (def_echo, handler_echo) = registry.get("mcp_happy_echo").expect("still registered");
    assert_eq!(def_echo.description, "native");
    let ctx = null_context();
    let out = handler_echo
        .call(&ctx, serde_json::json!({}))
        .await
        .expect("call");
    assert_eq!(out.as_str().unwrap(), "native");
    assert!(registry.contains("mcp_happy_ping"));

    mgr.shutdown_all().await;
}

#[tokio::test]
async fn read_resource_handler_returns_text() {
    let mgr = manager_with(vec![server_config("fs", "resources")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = McpToolCatalog::build(rt.clients().into_iter().map(|(_, c)| c).collect()).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);

    let (_, handler) = registry.get("mcp_fs_read_resource").expect("registered");
    let ctx = null_context();
    let out = handler
        .call(&ctx, serde_json::json!({"uri":"file:///readme"}))
        .await
        .expect("call");
    assert_eq!(out.as_str().unwrap(), "hello");
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn read_resource_missing_uri_arg_errors() {
    let mgr = manager_with(vec![server_config("fs", "resources")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = McpToolCatalog::build(rt.clients().into_iter().map(|(_, c)| c).collect()).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);

    let (_, handler) = registry.get("mcp_fs_read_resource").expect("registered");
    let ctx = null_context();
    let err = handler.call(&ctx, serde_json::json!({})).await.unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("missing") && msg.contains("uri"));
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn list_resource_templates_handler_returns_array() {
    let mgr = manager_with(vec![server_config("fs", "resources")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = McpToolCatalog::build(rt.clients().into_iter().map(|(_, c)| c).collect()).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);

    let (_, handler) = registry
        .get("mcp_fs_list_resource_templates")
        .expect("registered");
    let ctx = null_context();
    let out = handler
        .call(&ctx, serde_json::json!({}))
        .await
        .expect("call");
    let arr = out.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert!(arr
        .iter()
        .any(|v| v["uri_template"] == "file:///{path}" && v["name"] == "file"));
    assert!(arr
        .iter()
        .any(|v| v["uri_template"] == "log://{date}/{level}"));
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn list_resources_surfaces_annotations() {
    let mgr = manager_with(vec![server_config("fs", "resources")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = McpToolCatalog::build(rt.clients().into_iter().map(|(_, c)| c).collect()).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);

    let (_, handler) = registry.get("mcp_fs_list_resources").expect("registered");
    let ctx = null_context();
    let out = handler
        .call(&ctx, serde_json::json!({}))
        .await
        .expect("call");
    let arr = out.as_array().expect("array");
    let readme = arr
        .iter()
        .find(|v| v["uri"] == "file:///readme")
        .expect("readme present");
    let ann = &readme["annotations"];
    assert!(ann.is_object(), "annotations missing: {readme:?}");
    assert_eq!(ann["audience"][0], "assistant");
    assert!(ann["priority"].as_f64().unwrap() > 0.8);
    // Resource without annotations must not carry the key.
    let blob = arr
        .iter()
        .find(|v| v["uri"] == "file:///blob")
        .expect("blob present");
    assert!(blob.get("annotations").is_none());
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn read_resource_hits_cache_on_repeated_read() {
    use agent_core::agent::register_session_tools_with_overrides;

    let mgr = manager_with_cache(
        vec![server_config("fs", "resources")],
        agent_mcp::ResourceCacheConfig {
            enabled: true,
            ttl: Duration::from_secs(60),
            max_entries: 32,
        },
    );
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;

    let registry = ToolRegistry::new();
    register_session_tools_with_overrides(&rt, &registry, false, HashMap::new()).await;

    let (_, handler) = registry.get("mcp_fs_read_resource").expect("registered");
    let ctx = null_context();

    let first = handler
        .call(&ctx, serde_json::json!({"uri":"file:///readme"}))
        .await
        .expect("first call");
    assert_eq!(first.as_str().unwrap(), "hello");

    let cache = rt.resource_cache();
    assert!(cache.is_enabled());
    assert_eq!(cache.hits(), 0);

    let second = handler
        .call(&ctx, serde_json::json!({"uri":"file:///readme"}))
        .await
        .expect("second call");
    assert_eq!(second.as_str().unwrap(), "hello");
    assert_eq!(cache.hits(), 1, "second read should be a cache hit");

    // invalidate_server drops entry so next call is a miss again.
    let purged = cache.invalidate_server("fs");
    assert_eq!(purged, 1);
    let third = handler
        .call(&ctx, serde_json::json!({"uri":"file:///readme"}))
        .await
        .expect("third call");
    assert_eq!(third.as_str().unwrap(), "hello");
    assert_eq!(cache.hits(), 1, "post-invalidate read must not hit");

    mgr.shutdown_all().await;
}

#[tokio::test]
async fn read_resource_uri_outside_allowlist_counts_violation() {
    use agent_core::agent::register_session_tools_with_overrides;

    let mgr = manager_with_cache_and_allowlist(
        vec![server_config("fsviol", "resources")],
        Default::default(),
        vec!["db".to_string()],
    );
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;

    let registry = ToolRegistry::new();
    register_session_tools_with_overrides(&rt, &registry, false, HashMap::new()).await;

    let (_, handler) = registry
        .get("mcp_fsviol_read_resource")
        .expect("registered");
    let ctx = null_context();
    let _ = handler
        .call(&ctx, serde_json::json!({"uri":"file:///readme"}))
        .await
        .expect("call still succeeds (permissive)");

    let body = agent_mcp::telemetry::render_prometheus();
    assert!(
        body.contains("mcp_resource_uri_allowlist_violations_total{server=\"fsviol\"} 1"),
        "expected violation counter incremented, body:\n{body}"
    );
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn read_resource_uri_in_allowlist_no_violation() {
    use agent_core::agent::register_session_tools_with_overrides;

    let mgr = manager_with_cache_and_allowlist(
        vec![server_config("fsok", "resources")],
        Default::default(),
        vec!["file".to_string()],
    );
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;

    let registry = ToolRegistry::new();
    register_session_tools_with_overrides(&rt, &registry, false, HashMap::new()).await;

    let (_, handler) = registry.get("mcp_fsok_read_resource").expect("registered");
    let ctx = null_context();
    let _ = handler
        .call(&ctx, serde_json::json!({"uri":"file:///readme"}))
        .await
        .expect("call");

    let body = agent_mcp::telemetry::render_prometheus();
    assert!(
        !body.contains("mcp_resource_uri_allowlist_violations_total{server=\"fsok\"}"),
        "unexpected violation counter, body:\n{body}"
    );
    mgr.shutdown_all().await;
}

#[tokio::test]
async fn read_resource_cache_disabled_by_default() {
    use agent_core::agent::register_session_tools_with_overrides;

    let mgr = manager_with(vec![server_config("fs", "resources")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;

    let registry = ToolRegistry::new();
    register_session_tools_with_overrides(&rt, &registry, false, HashMap::new()).await;

    let (_, handler) = registry.get("mcp_fs_read_resource").expect("registered");
    let ctx = null_context();
    handler
        .call(&ctx, serde_json::json!({"uri":"file:///readme"}))
        .await
        .expect("call");
    handler
        .call(&ctx, serde_json::json!({"uri":"file:///readme"}))
        .await
        .expect("call");

    let cache = rt.resource_cache();
    assert!(!cache.is_enabled());
    assert_eq!(cache.hits(), 0);

    mgr.shutdown_all().await;
}

#[tokio::test]
async fn templates_tool_skipped_when_capability_absent() {
    let mgr = manager_with(vec![server_config("fs", "happy")]);
    let sid = Uuid::new_v4();
    let rt = mgr.get_or_create(sid).await;
    let catalog = McpToolCatalog::build(rt.clients().into_iter().map(|(_, c)| c).collect()).await;
    let registry = ToolRegistry::new();
    catalog.register_into(&registry);
    let names: Vec<String> = registry
        .to_tool_defs()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(!names.iter().any(|n| n.contains("_list_resource_templates")));
    mgr.shutdown_all().await;
}
