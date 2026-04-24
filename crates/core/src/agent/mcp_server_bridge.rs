//! Phase 12.6 — adapt `ToolRegistry` to `McpServerHandler`. Powers the
//! `agent mcp-server` subcommand so external MCP clients (Claude Desktop,
//! Cursor) can call the agent's registered tools.
use super::context::AgentContext;
use super::tool_registry::ToolRegistry;
use agent_mcp::{McpContent, McpTool, McpToolResult};
use agent_mcp::{McpError, McpServerHandler, McpServerInfo};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
pub struct ToolRegistryBridge {
    server_info: McpServerInfo,
    registry: Arc<ToolRegistry>,
    ctx: AgentContext,
    allowlist: Option<HashSet<String>>,
    expose_proxies: bool,
}
impl ToolRegistryBridge {
    pub fn new(
        server_info: McpServerInfo,
        registry: Arc<ToolRegistry>,
        ctx: AgentContext,
        allowlist: Option<HashSet<String>>,
        expose_proxies: bool,
    ) -> Self {
        Self {
            server_info,
            registry,
            ctx,
            allowlist,
            expose_proxies,
        }
    }
    fn is_proxy_tool(name: &str) -> bool {
        name.starts_with("ext_") || name.starts_with("mcp_")
    }
    fn is_allowed(&self, name: &str) -> bool {
        match &self.allowlist {
            Some(set) => set.contains(name),
            None => self.expose_proxies || !Self::is_proxy_tool(name),
        }
    }
    /// Convert a tool handler result into the MCP tool_result envelope.
    fn wrap_ok(value: Value) -> McpToolResult {
        let text = match value {
            Value::String(s) => s,
            other => serde_json::to_string_pretty(&other).unwrap_or_default(),
        };
        McpToolResult {
            content: vec![McpContent::Text { text }],
            is_error: false,
        }
    }
    fn wrap_err(err: &anyhow::Error) -> McpToolResult {
        McpToolResult {
            content: vec![McpContent::Text {
                text: format!("{err}"),
            }],
            is_error: true,
        }
    }
}
#[async_trait]
impl McpServerHandler for ToolRegistryBridge {
    fn server_info(&self) -> McpServerInfo {
        self.server_info.clone()
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let defs = self.registry.to_tool_defs();
        let tools: Vec<McpTool> = defs
            .into_iter()
            .filter(|d| self.is_allowed(&d.name))
            .map(|d| McpTool {
                name: d.name,
                description: Some(d.description),
                input_schema: d.parameters,
            })
            .collect();
        Ok(tools)
    }
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError> {
        if !self.is_allowed(name) {
            return Err(McpError::Protocol(format!(
                "tool '{name}' is not allowed by the mcp_server allowlist"
            )));
        }
        let (_def, handler) = self
            .registry
            .get(name)
            .ok_or_else(|| McpError::Protocol(format!("tool '{name}' is not registered")))?;
        match handler.call(&self.ctx, arguments).await {
            Ok(v) => Ok(Self::wrap_ok(v)),
            Err(e) => Ok(Self::wrap_err(&e)),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::super::tool_registry::ToolHandler;
    use super::*;
    use crate::session::SessionManager;
    use agent_broker::AnyBroker;
    use agent_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig,
    };
    use agent_llm::ToolDef;
    use async_trait::async_trait;
    struct FixedHandler {
        result: Result<Value, String>,
    }
    #[async_trait]
    impl ToolHandler for FixedHandler {
        async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            match &self.result {
                Ok(v) => Ok(v.clone()),
                Err(msg) => Err(anyhow::anyhow!(msg.clone())),
            }
        }
    }
    fn ctx() -> AgentContext {
        let cfg = Arc::new(AgentConfig {
            id: "test".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m".into(),
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
        });
        let broker = AnyBroker::local();
        let sessions = Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 20));
        AgentContext::new("test", cfg, broker, sessions)
    }
    fn def(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: "test".into(),
            parameters: serde_json::json!({"type":"object" }),
        }
    }
    fn build_bridge(
        allowlist: Option<HashSet<String>>,
        expose_proxies: bool,
    ) -> (ToolRegistryBridge, Arc<ToolRegistry>) {
        let registry = Arc::new(ToolRegistry::new());
        let info = McpServerInfo {
            name: "agent".into(),
            version: "0.1.0".into(),
        };
        let bridge =
            ToolRegistryBridge::new(info, registry.clone(), ctx(), allowlist, expose_proxies);
        (bridge, registry)
    }
    #[tokio::test]
    async fn list_tools_no_allowlist_returns_all() {
        let (bridge, registry) = build_bridge(None, true);
        registry.register(
            def("a"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        registry.register(
            def("b"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        let tools = bridge.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2);
    }
    #[tokio::test]
    async fn list_tools_filters_by_allowlist() {
        let mut allow = HashSet::new();
        allow.insert("a".to_string());
        let (bridge, registry) = build_bridge(Some(allow), false);
        registry.register(
            def("a"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        registry.register(
            def("b"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        let tools = bridge.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "a");
    }
    #[tokio::test]
    async fn call_tool_wraps_value_as_text() {
        let (bridge, registry) = build_bridge(None, true);
        registry.register(
            def("echo"),
            FixedHandler {
                result: Ok(Value::String("hi".into())),
            },
        );
        let result = bridge
            .call_tool("echo", serde_json::json!({}))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            McpContent::Text { text } => assert_eq!(text, "hi"),
            _ => panic!("expected text"),
        }
    }
    #[tokio::test]
    async fn call_tool_error_surfaces_as_is_error_true() {
        let (bridge, registry) = build_bridge(None, true);
        registry.register(
            def("broken"),
            FixedHandler {
                result: Err("boom".into()),
            },
        );
        let result = bridge
            .call_tool("broken", serde_json::json!({}))
            .await
            .unwrap();
        assert!(result.is_error);
    }
    #[tokio::test]
    async fn call_tool_unknown_returns_protocol_error() {
        let (bridge, _registry) = build_bridge(None, true);
        let err = bridge
            .call_tool("ghost", serde_json::json!({}))
            .await
            .unwrap_err();
        match err {
            McpError::Protocol(msg) => assert!(msg.contains("not registered")),
            other => panic!("unexpected: {other:?}"),
        }
    }
    #[tokio::test]
    async fn call_tool_outside_allowlist_returns_protocol_error() {
        let mut allow = HashSet::new();
        allow.insert("a".to_string());
        let (bridge, registry) = build_bridge(Some(allow), false);
        registry.register(
            def("b"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        let err = bridge
            .call_tool("b", serde_json::json!({}))
            .await
            .unwrap_err();
        match err {
            McpError::Protocol(msg) => assert!(msg.contains("allowlist")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_tools_hides_proxy_tools_by_default_without_allowlist() {
        let (bridge, registry) = build_bridge(None, false);
        registry.register(
            def("who_am_i"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        registry.register(
            def("ext_weather_ping"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        registry.register(
            def("mcp_brave_search"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        let tools = bridge.list_tools().await.unwrap();
        let names: Vec<String> = tools.into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["who_am_i".to_string()]);
    }

    #[tokio::test]
    async fn explicit_allowlist_can_expose_proxy_tool_even_when_expose_proxies_false() {
        let mut allow = HashSet::new();
        allow.insert("ext_weather_ping".to_string());
        let (bridge, registry) = build_bridge(Some(allow), false);
        registry.register(
            def("ext_weather_ping"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        let tools = bridge.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "ext_weather_ping");
    }
}
