//! Phase 80.9 — `channel_list` LLM tool.
//!
//! Lets the agent introspect which MCP channel servers are
//! currently registered for its binding. Read-only, idempotent
//! safe to auto-approve. Returns one [`nexo_mcp::channel::ChannelSummary`]
//! per registered server.
//!
//! Wiring lives in the boot path: when the agent's binding has at
//! least one entry in `allowed_channel_servers` (or the operator's
//! `agents.channels.enabled` is on for the binding's agent),
//! [`register_channel_list_tool`] adds this tool to the registry.

use anyhow::Result;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use nexo_mcp::channel::{ChannelSummary, SharedChannelRegistry};
use serde_json::{json, Value};
use std::sync::Arc;

use super::tool_registry::{ToolHandler, ToolRegistry};
use super::AgentContext;

pub const TOOL_NAME: &str = "channel_list";

/// Phase 80.9.j — resolve the binding id from the agent context.
/// Returns the active binding's `<plugin>:<instance>` when
/// `ctx.effective.binding_index` points to a valid entry; falls
/// back to the agent id for paths without a binding match
/// (heartbeat, delegate receive, tests). Pure-fn so tools can
/// share the same lookup without duplicating logic.
pub fn resolve_binding_id(ctx: &AgentContext) -> String {
    if let Some(eff) = ctx.effective.as_ref() {
        if let Some(idx) = eff.binding_index {
            if let Some(b) = ctx.config.inbound_bindings.get(idx) {
                return format!(
                    "{}:{}",
                    b.plugin,
                    b.instance.as_deref().unwrap_or("default")
                );
            }
        }
    }
    ctx.agent_id.clone()
}

#[derive(Clone)]
pub struct ChannelListTool {
    registry: SharedChannelRegistry,
    /// Phase 80.9.j — when `Some`, force this binding id (used by
    /// callers that own a specific binding context). When `None`
    /// the tool resolves from `ctx` at call time.
    binding_id: Option<String>,
}

impl ChannelListTool {
    /// Static-binding constructor used by callers that already
    /// know which binding their tool surface scopes to.
    pub fn new(registry: SharedChannelRegistry, binding_id: impl Into<String>) -> Self {
        Self {
            registry,
            binding_id: Some(binding_id.into()),
        }
    }

    /// Phase 80.9.j — dynamic-binding constructor. The tool reads
    /// `ctx.effective` at call time so the same registration
    /// serves every binding for an agent. Operators that want
    /// agent-scoped registration (the slim MVP shipped at
    /// 80.9 main.rs hookup) can keep using `new(registry, agent_id)`.
    pub fn new_dynamic(registry: SharedChannelRegistry) -> Self {
        Self {
            registry,
            binding_id: None,
        }
    }

    fn resolved_binding_id(&self, ctx: &AgentContext) -> String {
        self.binding_id
            .clone()
            .unwrap_or_else(|| resolve_binding_id(ctx))
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: TOOL_NAME.into(),
            description:
                "List MCP channel servers currently registered for this binding. \
                 Each entry is a Slack/Telegram/iMessage-style inbound surface — \
                 a server that can push user messages into your conversation \
                 via a `notifications/nexo/channel` notification. Use to learn \
                 which platforms can reach you, which plugin sources back them, \
                 and whether any of them can also relay permission prompts."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for ChannelListTool {
    async fn call(&self, ctx: &AgentContext, _args: Value) -> Result<Value> {
        let binding_id = self.resolved_binding_id(ctx);
        let entries = self.registry.list_for_binding(&binding_id).await;
        let summaries: Vec<ChannelSummary> = entries.iter().map(Into::into).collect();
        Ok(json!({
            "binding_id": binding_id,
            "count": summaries.len(),
            "servers": summaries
        }))
    }
}

/// Boot-time helper: register `channel_list` on `registry` when
/// the binding has any channel surface configured.
pub fn register_channel_list_tool(
    tools: &Arc<ToolRegistry>,
    channels: SharedChannelRegistry,
    binding_id: impl Into<String>,
) {
    let def = ChannelListTool::tool_def();
    let handler = Arc::new(ChannelListTool::new(channels, binding_id));
    tools.register_arc(def, handler);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_mcp::channel::{ChannelRegistry, RegisteredChannel};

    fn registered(binding: &str, server: &str) -> RegisteredChannel {
        RegisteredChannel {
            binding_id: binding.into(),
            server_name: server.into(),
            plugin_source: None,
            outbound_tool_name: None,
            permission_relay: false,
            registered_at_ms: 0,
        }
    }

    #[tokio::test]
    async fn empty_registry_returns_zero_count() {
        let reg: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let tool = ChannelListTool::new(reg, "wp:default");
        // We bypass AgentContext by calling the inner logic
        // directly through the public API surface.
        let entries = tool.registry.list_for_binding("wp:default").await;
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn registry_with_entries_renders_summaries() {
        let reg: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        reg.register(registered("wp:default", "slack")).await;
        reg.register(registered("wp:default", "telegram")).await;
        reg.register(registered("other:bind", "slack")).await;
        let tool = ChannelListTool::new(reg, "wp:default");
        let entries = tool.registry.list_for_binding("wp:default").await;
        let summaries: Vec<ChannelSummary> = entries.iter().map(Into::into).collect();
        assert_eq!(summaries.len(), 2);
        assert!(summaries.iter().any(|s| s.server_name == "slack"));
        assert!(summaries.iter().any(|s| s.server_name == "telegram"));
    }

    #[test]
    fn tool_def_has_stable_name_and_no_required_params() {
        let def = ChannelListTool::tool_def();
        assert_eq!(def.name, TOOL_NAME);
        // No `required` array → empty input is valid.
        assert!(def.parameters["properties"].as_object().unwrap().is_empty());
    }

    // ---- Phase 80.9.j resolver ----

    #[tokio::test]
    async fn dynamic_constructor_uses_static_when_set() {
        let reg: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let tool = ChannelListTool::new(reg, "wp:default");
        // Without a real ctx we just assert the field was stored.
        assert_eq!(tool.binding_id.as_deref(), Some("wp:default"));
    }

    #[tokio::test]
    async fn dynamic_constructor_unset_static_means_resolve_at_call_time() {
        let reg: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let tool = ChannelListTool::new_dynamic(reg);
        assert!(tool.binding_id.is_none());
    }
}
