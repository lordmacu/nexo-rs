//! Phase 80.9 — `channel_status` LLM tool.
//!
//! Read-only introspection of the channel registry for one
//! server: connection state (registered or not), capabilities
//! (permission relay opt-in), plugin source, registered-at
//! timestamp. Useful for the agent to ask itself "is Slack
//! reachable right now?" without bash-ing the operator.
//!
//! Distinct from `channel_list` (catalogue) and `channel_send`
//! (action) — this is the diagnostic surface.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_llm::ToolDef;
use nexo_mcp::channel::SharedChannelRegistry;
use serde_json::{json, Value};
use std::sync::Arc;

use super::tool_registry::{ToolHandler, ToolRegistry};
use super::AgentContext;

pub const TOOL_NAME: &str = "channel_status";

#[derive(Clone)]
pub struct ChannelStatusTool {
    registry: SharedChannelRegistry,
    /// Phase 80.9.j — `None` => resolve from `ctx.effective` at call time.
    binding_id: Option<String>,
}

impl ChannelStatusTool {
    pub fn new(registry: SharedChannelRegistry, binding_id: impl Into<String>) -> Self {
        Self {
            registry,
            binding_id: Some(binding_id.into()),
        }
    }

    /// Phase 80.9.j — dynamic-binding constructor.
    pub fn new_dynamic(registry: SharedChannelRegistry) -> Self {
        Self {
            registry,
            binding_id: None,
        }
    }

    fn resolved_binding_id(&self, ctx: &AgentContext) -> String {
        self.binding_id
            .clone()
            .unwrap_or_else(|| {
                super::channel_list_tool::resolve_binding_id(ctx)
            })
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: TOOL_NAME.into(),
            description:
                "Diagnose a registered MCP channel server. Returns whether the \
                 server is currently registered for this binding, its plugin \
                 source (when loaded via a plugin), the resolved outbound tool \
                 name, whether the server can relay permission prompts \
                 structurally, and the wallclock time the registration came up. \
                 When `server` is omitted the tool returns a status row per \
                 registered server."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {
                        "type": "string",
                        "description": "Optional server name. When omitted, every \
                                        registered server is returned."
                    }
                },
                "additionalProperties": false
            }),
        }
    }
}

fn render_status(reg: &nexo_mcp::channel::RegisteredChannel) -> Value {
    json!({
        "binding_id": reg.binding_id,
        "server_name": reg.server_name,
        "registered": true,
        "plugin_source": reg.plugin_source,
        "outbound_tool_name": reg.outbound_tool_name.clone()
            .unwrap_or_else(|| nexo_config::types::channels::DEFAULT_OUTBOUND_TOOL_NAME.to_string()),
        "permission_relay": reg.permission_relay,
        "registered_at_ms": reg.registered_at_ms,
    })
}

#[async_trait]
impl ToolHandler for ChannelStatusTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> Result<Value> {
        let binding_id = self.resolved_binding_id(ctx);
        match args["server"].as_str() {
            Some(s) if !s.is_empty() => {
                let entry = self.registry.get(&binding_id, s).await;
                match entry {
                    Some(r) => Ok(render_status(&r)),
                    None => Ok(json!({
                        "binding_id": binding_id,
                        "server_name": s,
                        "registered": false,
                    })),
                }
            }
            Some(_) => Err(anyhow!("channel_status: 'server' must be non-empty")),
            None => {
                let entries = self.registry.list_for_binding(&binding_id).await;
                let rendered: Vec<Value> = entries.iter().map(render_status).collect();
                Ok(json!({
                    "binding_id": binding_id,
                    "count": rendered.len(),
                    "servers": rendered,
                }))
            }
        }
    }
}

/// Boot helper: register `channel_status` for a binding.
pub fn register_channel_status_tool(
    tools: &Arc<ToolRegistry>,
    channels: SharedChannelRegistry,
    binding_id: impl Into<String>,
) {
    let def = ChannelStatusTool::tool_def();
    let handler = Arc::new(ChannelStatusTool::new(channels, binding_id));
    tools.register_arc(def, handler);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_mcp::channel::{ChannelRegistry, RegisteredChannel};

    fn registered(server: &str, outbound: Option<&str>, plugin_src: Option<&str>) -> RegisteredChannel {
        RegisteredChannel {
            binding_id: "b".into(),
            server_name: server.into(),
            plugin_source: plugin_src.map(str::to_string),
            outbound_tool_name: outbound.map(str::to_string),
            permission_relay: false,
            registered_at_ms: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn render_known_server_returns_full_row() {
        let r = registered("slack", Some("chat.postMessage"), Some("slack@anthropic"));
        let v = render_status(&r);
        assert_eq!(v["server_name"], "slack");
        assert_eq!(v["registered"], true);
        assert_eq!(v["outbound_tool_name"], "chat.postMessage");
        assert_eq!(v["plugin_source"], "slack@anthropic");
    }

    #[tokio::test]
    async fn render_falls_back_to_default_outbound_tool_name() {
        let r = registered("slack", None, None);
        let v = render_status(&r);
        assert_eq!(
            v["outbound_tool_name"],
            nexo_config::types::channels::DEFAULT_OUTBOUND_TOOL_NAME
        );
    }

    #[tokio::test]
    async fn unknown_server_returns_registered_false() {
        let reg: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let tool = ChannelStatusTool::new(reg, "b");
        let entry = tool.registry.get("b", "nope").await;
        assert!(entry.is_none());
    }

    #[test]
    fn tool_def_no_required_params() {
        let def = ChannelStatusTool::tool_def();
        assert!(!def.parameters.as_object().unwrap().contains_key("required"));
    }
}
