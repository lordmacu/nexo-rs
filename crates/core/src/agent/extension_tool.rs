//! Phase 11.5 — bridge an extension's JSON-RPC tools into the agent's
//! `ToolRegistry`. The LLM sees the tool (prefixed + attributed in the
//! description); calls are routed to the owning `StdioRuntime`.
use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use agent_extensions::{StdioRuntime, ToolDescriptor};
use agent_llm::ToolDef;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
/// Prefix prepended to every extension-provided tool name so they cannot
/// collide with native tools like `memory`, `heartbeat`, `who_am_i`.
pub const EXT_NAME_PREFIX: &str = "ext_";
pub struct ExtensionTool {
    plugin_id: String,
    tool_name: String,
    runtime: Arc<StdioRuntime>,
    context_passthrough: bool,
    /// Populated when constructed via `with_descriptor` so hooks can
    /// inspect what the extension advertised. Falls back to empty when
    /// the caller only had name + runtime handy.
    description: Option<String>,
    input_schema: Option<Value>,
}
impl ExtensionTool {
    pub fn new(
        plugin_id: impl Into<String>,
        tool_name: impl Into<String>,
        runtime: Arc<StdioRuntime>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            tool_name: tool_name.into(),
            runtime,
            context_passthrough: false,
            description: None,
            input_schema: None,
        }
    }
    /// Attach `description` + `input_schema` after construction so
    /// lifecycle hooks / introspection code can read what the extension
    /// advertised at handshake time. Idempotent.
    pub fn with_descriptor_metadata(
        mut self,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        self.description = Some(description.into());
        self.input_schema = Some(input_schema);
        self
    }
    /// Phase 11.5 follow-up — builder to opt this tool into context
    /// propagation. When true, `call` injects `_meta.agent_id` and
    /// `_meta.session_id` into the args object before forwarding.
    pub fn with_context_passthrough(mut self, enabled: bool) -> Self {
        self.context_passthrough = enabled;
        self
    }
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }
    /// The description the extension advertised at registration time.
    /// `None` when the tool was constructed without a descriptor (e.g.
    /// legacy call sites).
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    /// The `input_schema` (JSON Schema) the extension advertised at
    /// registration time. `None` when the tool was constructed without
    /// a descriptor.
    pub fn input_schema(&self) -> Option<&Value> {
        self.input_schema.as_ref()
    }
    /// Full LLM-facing tool name: `ext_{plugin_id}_{tool_name}`. When the
    /// concatenation overflows `ToolDef::MAX_NAME_LEN`, falls back to a
    /// deterministic `ext_{id}_{head}_{hash6}` form — see
    /// `ToolDef::fit_name`. Routing still uses the original `tool_name`.
    pub fn prefixed_name(plugin_id: &str, tool_name: &str) -> String {
        ToolDef::fit_name(EXT_NAME_PREFIX, plugin_id, tool_name)
    }
    /// Build a `ToolDef` ready for `ToolRegistry::register`. Decorates the
    /// description with `[ext:<id>]` so the LLM knows where the tool comes
    /// from when reasoning over its options.
    pub fn tool_def(desc: &ToolDescriptor, plugin_id: &str) -> ToolDef {
        ToolDef {
            name: Self::prefixed_name(plugin_id, &desc.name),
            description: format!("[ext:{plugin_id}] {}", desc.description),
            parameters: desc.input_schema.clone(),
        }
    }
}
/// Inject `_meta = { agent_id, session_id }` into `args` when
/// `passthrough` is true and `args` is a JSON object. No-op otherwise
/// (non-object payloads pass through unchanged). Extracted as a pure
/// function so it is unit-testable without a live runtime.
pub(crate) fn inject_context_meta(
    passthrough: bool,
    ctx: &AgentContext,
    args: Value,
    plugin_id: &str,
    tool_name: &str,
) -> Value {
    if !passthrough {
        return args;
    }
    let mut args = args;
    match args.as_object_mut() {
        Some(obj) => {
            obj.insert(
                "_meta".into(),
                serde_json::json!({
                    "agent_id": ctx.agent_id,
                    "session_id": ctx.session_id.map(|u| u.to_string()),
                }),
            );
        }
        None => tracing::debug!(
            ext = %plugin_id,
            tool = %tool_name,
            "context_passthrough skipped: args is not an object"
        ),
    }
    args
}
#[async_trait]
impl ToolHandler for ExtensionTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let args = inject_context_meta(
            self.context_passthrough,
            ctx,
            args,
            &self.plugin_id,
            &self.tool_name,
        );
        self.runtime
            .tools_call(&self.tool_name, args)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "extension `{}` tool `{}` failed: {e}",
                    self.plugin_id,
                    self.tool_name
                )
            })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use agent_broker::AnyBroker;
    use agent_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig,
    };
    use std::time::Duration;
    use uuid::Uuid;
    fn test_ctx(agent: &str, session: Option<Uuid>) -> AgentContext {
        let cfg = Arc::new(AgentConfig {
            id: agent.into(),
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
            language: None,
        });
        let broker = AnyBroker::local();
        let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
        let ctx = AgentContext::new(agent, cfg, broker, sessions);
        match session {
            Some(id) => ctx.with_session_id(id),
            None => ctx,
        }
    }
    #[tokio::test]
    async fn inject_meta_passthrough_off_returns_args_unchanged() {
        let ctx = test_ctx("kate", Some(Uuid::new_v4()));
        let args = serde_json::json!({"x": 1});
        let out = inject_context_meta(false, &ctx, args.clone(), "weather", "get");
        assert_eq!(out, args);
    }
    #[tokio::test]
    async fn inject_meta_on_object_adds_meta_field() {
        let sid = Uuid::new_v4();
        let ctx = test_ctx("kate", Some(sid));
        let args = serde_json::json!({"x": 1});
        let out = inject_context_meta(true, &ctx, args, "weather", "get");
        assert_eq!(out["x"], 1);
        assert_eq!(out["_meta"]["agent_id"], "kate");
        assert_eq!(out["_meta"]["session_id"], sid.to_string());
    }
    #[tokio::test]
    async fn inject_meta_session_none_serializes_null() {
        let ctx = test_ctx("kate", None);
        let out = inject_context_meta(true, &ctx, serde_json::json!({}), "weather", "get");
        assert_eq!(out["_meta"]["agent_id"], "kate");
        assert!(out["_meta"]["session_id"].is_null());
    }
    #[tokio::test]
    async fn inject_meta_scalar_args_passthrough_unchanged() {
        let ctx = test_ctx("kate", Some(Uuid::new_v4()));
        let out = inject_context_meta(
            true,
            &ctx,
            serde_json::json!("raw string"),
            "weather",
            "get",
        );
        assert_eq!(out, serde_json::json!("raw string"));
    }
    #[tokio::test]
    async fn inject_meta_overrides_existing_meta() {
        let ctx = test_ctx("kate", Some(Uuid::new_v4()));
        let args = serde_json::json!({"_meta": {"spoofed": true}});
        let out = inject_context_meta(true, &ctx, args, "weather", "get");
        assert_eq!(out["_meta"]["agent_id"], "kate");
        assert!(out["_meta"].get("spoofed").is_none());
    }
    #[test]
    fn prefixed_name_concatenates_prefix_id_and_tool() {
        assert_eq!(
            ExtensionTool::prefixed_name("weather", "get_forecast"),
            "ext_weather_get_forecast"
        );
    }
    #[test]
    fn tool_def_decorates_description() {
        let desc = ToolDescriptor {
            name: "echo".into(),
            description: "Echoes its input".into(),
            input_schema: serde_json::json!({"type":"object"}),
        };
        let def = ExtensionTool::tool_def(&desc, "util");
        assert_eq!(def.name, "ext_util_echo");
        assert_eq!(def.description, "[ext:util] Echoes its input");
        assert_eq!(def.parameters, serde_json::json!({"type":"object"}));
    }
    #[test]
    fn prefixed_name_passthrough_when_short() {
        assert_eq!(
            ExtensionTool::prefixed_name("weather", "get_forecast"),
            "ext_weather_get_forecast"
        );
    }
    #[test]
    fn prefixed_name_exactly_at_max_is_unchanged() {
        let id = "a".repeat(29);
        let tool = "b".repeat(30); // 4 + 29 + 1 + 30 = 64
        let name = ExtensionTool::prefixed_name(&id, &tool);
        assert_eq!(name.len(), 64);
        assert!(name.starts_with("ext_"));
        assert!(!name.contains("__"));
    }
    #[test]
    fn long_name_is_hashed_into_limit() {
        let id = "mybot";
        let tool = "very_long_tool_name_".repeat(10); // ~200 chars
        let name = ExtensionTool::prefixed_name(id, &tool);
        assert_eq!(name.len(), 64);
        assert!(name.starts_with("ext_mybot_"));
    }
    #[test]
    fn different_long_tools_yield_different_names() {
        let id = "mybot";
        let a = ExtensionTool::prefixed_name(id, &("aaa".repeat(50)));
        let b = ExtensionTool::prefixed_name(id, &("bbb".repeat(50)));
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);
    }
}
