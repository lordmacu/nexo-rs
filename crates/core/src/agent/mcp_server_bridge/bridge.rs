//! Phase 12.6 — adapt `ToolRegistry` to `McpServerHandler`. Powers the
//! `agent mcp-server` subcommand so external MCP clients (Claude Desktop,
//! Cursor) can call the agent's registered tools.
use crate::agent::context::AgentContext;
use crate::agent::tool_registry::ToolRegistry;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use nexo_mcp::{
    McpAnnotations, McpContent, McpError, McpPrompt, McpPromptArgument, McpPromptMessage,
    McpPromptResult, McpResource, McpResourceContent, McpServerHandler, McpServerInfo, McpTool,
    McpToolResult,
};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

const RESOURCE_TEXT_CHAR_LIMIT: usize = 32_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceDoc {
    Soul,
    Memory,
}

impl WorkspaceDoc {
    fn all() -> [Self; 2] {
        [Self::Soul, Self::Memory]
    }

    fn from_uri(uri: &str) -> Option<Self> {
        match uri {
            "agent://workspace/soul" => Some(Self::Soul),
            "agent://workspace/memory" => Some(Self::Memory),
            _ => None,
        }
    }

    fn from_prompt(name: &str) -> Option<Self> {
        match name {
            "workspace_soul_context" => Some(Self::Soul),
            "workspace_memory_context" => Some(Self::Memory),
            _ => None,
        }
    }

    fn uri(self) -> &'static str {
        match self {
            Self::Soul => "agent://workspace/soul",
            Self::Memory => "agent://workspace/memory",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Soul => "SOUL.md",
            Self::Memory => "MEMORY.md",
        }
    }

    fn prompt_name(self) -> &'static str {
        match self {
            Self::Soul => "workspace_soul_context",
            Self::Memory => "workspace_memory_context",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Soul => "Agent persona and behavior guidelines from SOUL.md",
            Self::Memory => "Consolidated long-term memory from MEMORY.md",
        }
    }

    fn annotation_priority(self) -> f32 {
        match self {
            Self::Soul => 0.95,
            Self::Memory => 0.9,
        }
    }
}
// Phase 76.1 — `Clone` lets the same bridge serve both the stdio
// and HTTP transports concurrently. All fields are cheap to clone
// (Arc, owned String, ArcSwap-shared allowlist).
//
// Phase M1 — `allowlist` is now `Arc<ArcSwap<...>>` so an external
// caller (post-hook / SIGHUP / admin-ui) can swap the filter
// without reconstructing the bridge. Clones share the same
// ArcSwap, so a swap on any clone is visible to all clones.
// `list_changed_capability` advertises the MCP `tools/listChanged`
// capability — `true` means the underlying transport can push
// `notifications/tools/list_changed` (HTTP/SSE today via
// `HttpServerHandle::notify_tools_list_changed`). Stdio cannot
// push server→client mid-session today, so the default is `false`
// and only the HTTP-bound clone flips it on.
//
// IRROMPIBLE refs (claude-code-leak):
// - `src/services/mcp/useManageMCPConnections.ts:618-665` — the
//   client-side handler is registered ONLY when the server
//   advertises `capabilities.tools.listChanged: true`. Advertising
//   `true` without ever emitting strands the client; advertising
//   `false` while emitting is a no-op. Cap and emit are coupled.
// - `:628-633` — consumer's invalidate-then-fetch pattern after a
//   notification. Mirror surface here is `swap_allowlist` followed
//   by `notify_tools_list_changed()` on the transport handle.
//
// Provider-agnostic: the MCP protocol-level capability is
// transversal across Anthropic / MiniMax / OpenAI / Gemini /
// DeepSeek / xAI / Mistral — none of them touch this negotiation.
#[derive(Clone)]
pub struct ToolRegistryBridge {
    server_info: McpServerInfo,
    registry: Arc<ToolRegistry>,
    ctx: AgentContext,
    /// Hot-swap-able expose_tools filter. Outer `None` means no
    /// filter (everything non-proxy is exposed); `Some(set)` means
    /// only listed names pass `is_allowed`. Swapped via
    /// `swap_allowlist`. Cloned bridges share the same `Arc` so a
    /// single swap is visible across stdio + HTTP clones.
    allowlist: Arc<ArcSwap<Option<Arc<HashSet<String>>>>>,
    /// Whether the bridge advertises `capabilities.tools.listChanged
    /// = true` to MCP clients. Default `false` — opt-in via
    /// `with_list_changed_capability(true)` only on transports that
    /// can emit the notification.
    list_changed_capability: bool,
    /// Reserved for future non-proxy categories. Today proxy tools
    /// (`ext_*`, `mcp_*`) are unconditionally filtered, so this flag
    /// is kept on the struct for source compatibility but doesn't
    /// affect filtering. See `is_allowed`.
    #[allow(dead_code)]
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
        let inner = allowlist.map(Arc::new);
        Self {
            server_info,
            registry,
            ctx,
            allowlist: Arc::new(ArcSwap::from_pointee(inner)),
            list_changed_capability: false,
            expose_proxies,
        }
    }

    /// Phase M1 — opt in to advertising `tools/listChanged: true`
    /// in the capability negotiation. Caller is responsible for
    /// actually pushing `notifications/tools/list_changed` (e.g.
    /// `HttpServerHandle::notify_tools_list_changed()`) when the
    /// effective tool set changes. Setting this on a transport that
    /// cannot push notifications strands clients waiting for an
    /// event that will never arrive — see the cap+emit coupling
    /// note in the struct doc.
    pub fn with_list_changed_capability(mut self, on: bool) -> Self {
        self.list_changed_capability = on;
        self
    }

    /// Phase M1 — atomically replace the allowlist filter. Future
    /// `list_tools` / `call_tool` calls observe the new set
    /// immediately. `None` removes the filter (everything non-proxy
    /// is exposed). Concurrent in-flight `is_allowed` calls finish
    /// against the previous snapshot — `arc_swap::ArcSwap` keeps
    /// the prior `Arc` alive until all readers drop it. Caller
    /// should pair this with a `notify_tools_list_changed()` call
    /// on the transport handle so subscribed clients refresh.
    pub fn swap_allowlist(&self, new: Option<HashSet<String>>) {
        let next = new.map(Arc::new);
        self.allowlist.store(Arc::new(next));
    }

    fn is_proxy_tool(name: &str) -> bool {
        name.starts_with("ext_") || name.starts_with("mcp_")
    }
    fn is_allowed(&self, name: &str) -> bool {
        // Proxy tools (`ext_*`, `mcp_*`) are NEVER exposed to external
        // MCP clients regardless of `expose_proxies`. Letting them
        // through would turn the agent into an open relay for any
        // sub-MCP server it has registered. The flag retains its
        // role for future non-proxy categories.
        if Self::is_proxy_tool(name) {
            return false;
        }
        // `arc_swap::Guard<Arc<Option<Arc<HashSet>>>>`. The `**`
        // descends through Guard + outer Arc to the `Option`.
        match &**self.allowlist.load() {
            Some(set) => set.contains(name),
            None => true,
        }
    }
    /// Convert a tool handler result into the MCP tool_result envelope.
    fn wrap_ok(value: Value) -> McpToolResult {
        let text = match &value {
            Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        // Phase 74.3 — populate `structured_content` only when the
        // payload is non-string (objects/arrays). Pure-string
        // results stay text-only because Claude wouldn't gain a
        // typed view from echoing the same string twice.
        let structured = match &value {
            Value::String(_) | Value::Null => None,
            _ => Some(value),
        };
        McpToolResult {
            content: vec![McpContent::Text { text }],
            is_error: false,
            structured_content: structured,
        }
    }
    fn wrap_err(err: &anyhow::Error) -> McpToolResult {
        McpToolResult {
            content: vec![McpContent::Text {
                text: format!("{err}"),
            }],
            is_error: true,
            structured_content: None,
        }
    }

    fn workspace_root(&self) -> Option<PathBuf> {
        let workspace = self.ctx.config.workspace.trim();
        if workspace.is_empty() {
            None
        } else {
            Some(PathBuf::from(workspace))
        }
    }

    async fn list_workspace_docs(&self) -> Vec<WorkspaceDoc> {
        let Some(root) = self.workspace_root() else {
            return Vec::new();
        };
        let mut docs = Vec::new();
        for doc in WorkspaceDoc::all() {
            let path = root.join(doc.file_name());
            if let Ok(meta) = tokio::fs::metadata(path).await {
                if meta.is_file() {
                    docs.push(doc);
                }
            }
        }
        docs
    }

    async fn read_workspace_doc(&self, doc: WorkspaceDoc) -> Result<String, McpError> {
        let Some(root) = self.workspace_root() else {
            return Err(McpError::Protocol(
                "workspace is not configured for this agent".into(),
            ));
        };
        let path = root.join(doc.file_name());
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(McpError::Protocol(format!(
                    "resource not found: {}",
                    doc.uri()
                )));
            }
            Err(e) => return Err(McpError::Transport(e)),
        };
        Ok(truncate_with_marker(&text, RESOURCE_TEXT_CHAR_LIMIT))
    }

    fn derive_session_uuid(raw: &str) -> Uuid {
        Uuid::parse_str(raw).unwrap_or_else(|_| Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.as_bytes()))
    }

    async fn call_tool_inner(
        &self,
        name: &str,
        arguments: Value,
        dispatch_ctx: Option<&nexo_mcp::server::DispatchContext>,
    ) -> Result<McpToolResult, McpError> {
        if !self.is_allowed(name) {
            return Err(McpError::Protocol(format!(
                "tool '{name}' is not allowed by the mcp_server allowlist"
            )));
        }
        let (_def, handler) = self
            .registry
            .get(name)
            .ok_or_else(|| McpError::Protocol(format!("tool '{name}' is not registered")))?;

        let call_ctx = if let Some(raw) = dispatch_ctx.and_then(|c| c.session_id.as_deref()) {
            self.ctx
                .clone()
                .with_session_id(Self::derive_session_uuid(raw))
        } else {
            self.ctx.clone()
        };
        match handler.call(&call_ctx, arguments).await {
            Ok(v) => Ok(Self::wrap_ok(v)),
            Err(e) => Ok(Self::wrap_err(&e)),
        }
    }
}

fn truncate_with_marker(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push_str("\n\n[truncated]");
    out
}
#[async_trait]
impl McpServerHandler for ToolRegistryBridge {
    fn server_info(&self) -> McpServerInfo {
        self.server_info.clone()
    }
    fn capabilities(&self) -> Value {
        // Phase M1 — `tools.listChanged` reflects whether THIS
        // bridge instance is bound to a transport that can push
        // `notifications/tools/list_changed`. Defaults to `false`;
        // HTTP path opts in via `with_list_changed_capability(true)`.
        // See struct doc for cap+emit coupling rationale.
        serde_json::json!({
            "tools": { "listChanged": self.list_changed_capability },
            "resources": { "listChanged": false },
            "prompts": { "listChanged": false },
        })
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
                output_schema: None,
            })
            .collect();
        Ok(tools)
    }
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError> {
        self.call_tool_inner(name, arguments, None).await
    }

    async fn call_tool_with_context(
        &self,
        name: &str,
        arguments: Value,
        ctx: &nexo_mcp::server::DispatchContext,
    ) -> Result<McpToolResult, McpError> {
        self.call_tool_inner(name, arguments, Some(ctx)).await
    }

    async fn call_tool_streaming_with_context(
        &self,
        name: &str,
        arguments: Value,
        progress: nexo_mcp::server::progress::ProgressReporter,
        ctx: &nexo_mcp::server::DispatchContext,
    ) -> Result<McpToolResult, McpError> {
        let _ = progress;
        self.call_tool_inner(name, arguments, Some(ctx)).await
    }

    async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
        let docs = self.list_workspace_docs().await;
        let resources = docs
            .into_iter()
            .map(|doc| McpResource {
                uri: doc.uri().to_string(),
                name: doc.file_name().to_string(),
                description: Some(doc.description().to_string()),
                mime_type: Some("text/markdown".to_string()),
                annotations: Some(McpAnnotations {
                    audience: vec!["assistant".to_string()],
                    priority: Some(doc.annotation_priority()),
                }),
            })
            .collect();
        Ok(resources)
    }

    async fn read_resource(&self, uri: &str) -> Result<Vec<McpResourceContent>, McpError> {
        let doc = WorkspaceDoc::from_uri(uri)
            .ok_or_else(|| McpError::Protocol(format!("resource not found: {uri}")))?;
        let text = self.read_workspace_doc(doc).await?;
        Ok(vec![McpResourceContent {
            uri: uri.to_string(),
            mime_type: Some("text/markdown".to_string()),
            text: Some(text),
            blob: None,
        }])
    }

    async fn list_prompts(&self) -> Result<Vec<McpPrompt>, McpError> {
        let docs = self.list_workspace_docs().await;
        let prompts = docs
            .into_iter()
            .map(|doc| McpPrompt {
                name: doc.prompt_name().to_string(),
                description: Some(format!(
                    "Build a prompt using {} as context",
                    doc.file_name()
                )),
                arguments: vec![McpPromptArgument {
                    name: "question".to_string(),
                    description: Some("Optional user question to append".to_string()),
                    required: false,
                }],
            })
            .collect();
        Ok(prompts)
    }

    async fn get_prompt(&self, name: &str, arguments: Value) -> Result<McpPromptResult, McpError> {
        let doc = WorkspaceDoc::from_prompt(name)
            .ok_or_else(|| McpError::Protocol(format!("prompt not found: {name}")))?;
        let text = self.read_workspace_doc(doc).await?;
        let question = arguments
            .get("question")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");

        let mut prompt_body = format!(
            "Use the following {} as context:\n\n```markdown\n{}\n```",
            doc.file_name(),
            text
        );
        if !question.is_empty() {
            prompt_body.push_str("\n\nQuestion:\n");
            prompt_body.push_str(question);
        }

        Ok(McpPromptResult {
            description: Some(format!("Context prompt from {}", doc.file_name())),
            messages: vec![McpPromptMessage {
                role: "user".to_string(),
                content: McpContent::Text { text: prompt_body },
            }],
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tool_registry::ToolHandler;
    use crate::session::SessionManager;
    use async_trait::async_trait;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig,
    };
    use nexo_llm::ToolDef;
    use tempfile::tempdir;
    struct FixedHandler {
        result: Result<Value, String>,
    }
    struct SessionEchoHandler;
    #[async_trait]
    impl ToolHandler for FixedHandler {
        async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            match &self.result {
                Ok(v) => Ok(v.clone()),
                Err(msg) => Err(anyhow::anyhow!(msg.clone())),
            }
        }
    }
    #[async_trait]
    impl ToolHandler for SessionEchoHandler {
        async fn call(&self, ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            Ok(serde_json::json!({
                "session_id": ctx.session_id.map(|s| s.to_string())
            }))
        }
    }
    fn ctx(workspace: Option<&std::path::Path>) -> AgentContext {
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
            workspace: workspace
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
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
        workspace: Option<&std::path::Path>,
    ) -> (ToolRegistryBridge, Arc<ToolRegistry>) {
        let registry = Arc::new(ToolRegistry::new());
        let info = McpServerInfo {
            name: "agent".into(),
            version: "0.1.0".into(),
        };
        let bridge = ToolRegistryBridge::new(
            info,
            registry.clone(),
            ctx(workspace),
            allowlist,
            expose_proxies,
        );
        (bridge, registry)
    }
    #[tokio::test]
    async fn list_tools_no_allowlist_returns_all() {
        let (bridge, registry) = build_bridge(None, true, None);
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
        let (bridge, registry) = build_bridge(Some(allow), false, None);
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
        let (bridge, registry) = build_bridge(None, true, None);
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
        let (bridge, registry) = build_bridge(None, true, None);
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
        let (bridge, _registry) = build_bridge(None, true, None);
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
        let (bridge, registry) = build_bridge(Some(allow), false, None);
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
    async fn call_tool_with_dispatch_context_injects_session_id() {
        let (bridge, registry) = build_bridge(None, false, None);
        registry.register(def("echo_session"), SessionEchoHandler);
        let sid = Uuid::new_v4().to_string();
        let mut dctx = nexo_mcp::server::DispatchContext::empty();
        dctx.session_id = Some(sid.clone());
        let out = bridge
            .call_tool_with_context("echo_session", serde_json::json!({}), &dctx)
            .await
            .unwrap();
        assert!(!out.is_error);
        let structured = out.structured_content.expect("structured content");
        assert_eq!(structured["session_id"], sid);
    }

    #[tokio::test]
    async fn list_tools_hides_proxy_tools_by_default_without_allowlist() {
        let (bridge, registry) = build_bridge(None, false, None);
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
    async fn explicit_allowlist_does_not_expose_proxy_tools() {
        // Proxy tools (ext_*, mcp_*) are now never exposed to external
        // MCP clients regardless of allowlist or `expose_proxies`.
        // Letting them through would turn the agent-as-MCP-server
        // into an open relay for any sub-MCP server it has wired.
        // See `is_allowed`.
        let mut allow = HashSet::new();
        allow.insert("ext_weather_ping".to_string());
        let (bridge, registry) = build_bridge(Some(allow), false, None);
        registry.register(
            def("ext_weather_ping"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        let tools = bridge.list_tools().await.unwrap();
        assert!(
            tools.iter().all(|t| !t.name.starts_with("ext_")),
            "proxy tools must be filtered even with explicit allowlist"
        );
    }

    #[tokio::test]
    async fn list_resources_exposes_workspace_docs() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("SOUL.md"), "be direct").unwrap();
        std::fs::write(tmp.path().join("MEMORY.md"), "## Facts\n- likes coffee\n").unwrap();
        let (bridge, _registry) = build_bridge(None, false, Some(tmp.path()));

        let resources = bridge.list_resources().await.unwrap();
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].uri, "agent://workspace/soul");
        assert_eq!(resources[1].uri, "agent://workspace/memory");
        assert_eq!(resources[0].mime_type.as_deref(), Some("text/markdown"));
    }

    #[tokio::test]
    async fn read_resource_returns_workspace_markdown() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("SOUL.md"), "voice + style").unwrap();
        let (bridge, _registry) = build_bridge(None, false, Some(tmp.path()));

        let contents = bridge
            .read_resource("agent://workspace/soul")
            .await
            .unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].uri, "agent://workspace/soul");
        assert!(contents[0]
            .text
            .as_deref()
            .unwrap_or("")
            .contains("voice + style"));
    }

    #[tokio::test]
    async fn prompts_list_and_get_use_workspace_docs() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("SOUL.md"), "be concise").unwrap();
        let (bridge, _registry) = build_bridge(None, false, Some(tmp.path()));

        let prompts = bridge.list_prompts().await.unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "workspace_soul_context");

        let prompt = bridge
            .get_prompt(
                "workspace_soul_context",
                serde_json::json!({"question":"How should you answer?"}),
            )
            .await
            .unwrap();
        assert_eq!(prompt.messages.len(), 1);
        let McpContent::Text { text } = &prompt.messages[0].content else {
            panic!("expected text content");
        };
        assert!(text.contains("be concise"));
        assert!(text.contains("How should you answer?"));
    }

    // ── Phase M1 — listChanged capability + hot-swap allowlist ──

    #[tokio::test]
    async fn capability_defaults_to_false() {
        let (bridge, _) = build_bridge(None, true, None);
        let caps = bridge.capabilities();
        assert_eq!(caps["tools"]["listChanged"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn with_list_changed_capability_flips_capability() {
        let (bridge, _) = build_bridge(None, true, None);
        let bridge = bridge.with_list_changed_capability(true);
        let caps = bridge.capabilities();
        assert_eq!(caps["tools"]["listChanged"], serde_json::json!(true));
        // Resources and prompts stay false — M1 only touches tools.
        assert_eq!(caps["resources"]["listChanged"], serde_json::json!(false));
        assert_eq!(caps["prompts"]["listChanged"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn swap_allowlist_visible_immediately() {
        let mut allow_a = HashSet::new();
        allow_a.insert("A".to_string());
        let (bridge, registry) = build_bridge(Some(allow_a), false, None);
        registry.register(
            def("A"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        registry.register(
            def("B"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );

        // Initial state: only A allowed.
        let tools = bridge.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "A");

        // Swap to {B}.
        let mut allow_b = HashSet::new();
        allow_b.insert("B".to_string());
        bridge.swap_allowlist(Some(allow_b));
        let tools = bridge.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "B");

        // Swap to None — both pass (no filter).
        bridge.swap_allowlist(None);
        let tools = bridge.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2);
    }

    #[tokio::test]
    async fn swap_allowlist_propagates_through_clone() {
        let mut allow_a = HashSet::new();
        allow_a.insert("A".to_string());
        let (bridge, registry) = build_bridge(Some(allow_a), false, None);
        registry.register(
            def("A"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        registry.register(
            def("B"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        let bridge_clone = bridge.clone();

        // Swap on the original — clone observes the new set
        // because both share the same `Arc<ArcSwap>`.
        let mut allow_b = HashSet::new();
        allow_b.insert("B".to_string());
        bridge.swap_allowlist(Some(allow_b));

        let tools_clone = bridge_clone.list_tools().await.unwrap();
        assert_eq!(tools_clone.len(), 1);
        assert_eq!(tools_clone[0].name, "B");
    }

    #[tokio::test]
    async fn proxy_tools_filtered_regardless_of_swap() {
        let (bridge, registry) = build_bridge(None, true, None);
        registry.register(
            def("ext_remote"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        registry.register(
            def("mcp_proxy"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );
        registry.register(
            def("normal"),
            FixedHandler {
                result: Ok(Value::Null),
            },
        );

        // Even with `None` allowlist (no filter) proxies are dropped.
        bridge.swap_allowlist(None);
        let tools = bridge.list_tools().await.unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["normal"]);
    }
}
