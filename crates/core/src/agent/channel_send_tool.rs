//! Phase 80.9 — `channel_send` LLM tool.
//!
//! Stable wrapper that lets the agent reply through a registered
//! MCP channel server without having to know the server's
//! outbound tool name. Resolution chain:
//!
//! 1. Look up the server in [`SharedChannelRegistry`].
//! 2. Read `RegisteredChannel.outbound_tool_name` (snapshot of
//!    the operator's `ApprovedChannel.outbound_tool_name` at
//!    register-time, falls back to
//!    [`nexo_config::types::channels::DEFAULT_OUTBOUND_TOOL_NAME`]).
//! 3. Invoke the resolved tool through
//!    [`SessionMcpRuntime::call_tool`] with the supplied
//!    `arguments` payload.
//!
//! Snapshotting the tool name at register-time means an in-flight
//! reply still hits the tool the operator approved when they
//! flipped the config. Hot-reload is observed on the next
//! registration cycle.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_llm::ToolDef;
use nexo_mcp::channel::SharedChannelRegistry;
use serde_json::{json, Value};
use std::sync::Arc;

use super::tool_registry::{ToolHandler, ToolRegistry};
use super::AgentContext;

pub const TOOL_NAME: &str = "channel_send";

/// Maximum content size accepted by `channel_send`. Defends the
/// downstream MCP server against an LLM-generated mega-payload
/// (no MCP server signs up to receive 1 MiB of "casual reply").
pub const MAX_CONTENT_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct ChannelSendTool {
    registry: SharedChannelRegistry,
    /// Phase 80.9.j — `None` => resolve from `ctx.effective` at call time.
    binding_id: Option<String>,
}

impl ChannelSendTool {
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
                "Send an outbound message through a registered MCP channel server \
                 (Slack, Telegram, iMessage, etc.). Resolves the server's outbound \
                 tool by name from the channel registry and invokes it with the \
                 provided arguments. Use `channel_list` first to discover which \
                 servers are reachable from this binding. The `arguments` object is \
                 passed through to the underlying tool verbatim — its shape is \
                 server-defined; common keys are `chat_id`, `thread_ts`, `text`, \
                 `attachments`. The `content` shortcut populates the most common \
                 field name (`text` or `content` depending on the server's schema)."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {
                        "type": "string",
                        "description": "Name of the registered MCP channel server."
                    },
                    "content": {
                        "type": "string",
                        "description": "Body of the reply. Wrapped into the \
                                        underlying tool's argument object as \
                                        `text` (or merged into `arguments` if \
                                        you supply both)."
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Optional pre-built argument object \
                                        passed verbatim to the underlying MCP \
                                        tool. Use this when the server expects \
                                        keys beyond plain content (e.g. \
                                        `chat_id`, `thread_ts`).",
                        "additionalProperties": true
                    }
                },
                "required": ["server"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for ChannelSendTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> Result<Value> {
        // ---- Gate 1: server present ----
        let server = args["server"]
            .as_str()
            .ok_or_else(|| anyhow!("channel_send: 'server' must be a non-empty string"))?
            .trim();
        if server.is_empty() {
            return Err(anyhow!("channel_send: 'server' must be a non-empty string"));
        }

        // ---- Gate 2: server is registered ----
        let binding_id = self.resolved_binding_id(ctx);
        let registered = self
            .registry
            .get(&binding_id, server)
            .await
            .ok_or_else(|| {
                anyhow!(
                    "channel_send: server '{}' is not registered for binding '{}' \
                     (run channel_list to see what's available)",
                    server,
                    binding_id
                )
            })?;

        // ---- Gate 3: resolve outbound tool name ----
        let tool_name = registered
            .outbound_tool_name
            .as_deref()
            .unwrap_or(nexo_config::types::channels::DEFAULT_OUTBOUND_TOOL_NAME);

        // ---- Gate 4: build arguments — content shortcut + size cap ----
        let mut tool_args = match &args["arguments"] {
            Value::Object(_) => args["arguments"].clone(),
            Value::Null => Value::Object(serde_json::Map::new()),
            _ => return Err(anyhow!("channel_send: 'arguments' must be an object")),
        };
        if let Some(content) = args["content"].as_str() {
            if content.len() > MAX_CONTENT_BYTES {
                return Err(anyhow!(
                    "channel_send: 'content' exceeds {} bytes",
                    MAX_CONTENT_BYTES
                ));
            }
            // Servers vary on the field name. Inject under `text`
            // if not already set; the server's schema validates
            // anyway. Operators who care can pass everything
            // through `arguments` directly.
            if let Value::Object(map) = &mut tool_args {
                if !map.contains_key("text") && !map.contains_key("content") {
                    map.insert("text".into(), Value::String(content.to_string()));
                }
            }
        }

        // ---- Gate 5: MCP runtime present ----
        let mcp = ctx.mcp.as_ref().ok_or_else(|| {
            anyhow!("channel_send: MCP runtime not wired into this AgentContext")
        })?;

        // ---- Invoke ----
        let result = mcp
            .call_tool(server, tool_name, tool_args.clone())
            .await
            .map_err(|e| anyhow!("channel_send: MCP call failed: {e}"))?;

        Ok(json!({
            "server": server,
            "tool_name": tool_name,
            "is_error": result.is_error,
            "content": result.content,
            "structured_content": result.structured_content,
        }))
    }
}

/// Boot helper: register `channel_send` for a binding.
pub fn register_channel_send_tool(
    tools: &Arc<ToolRegistry>,
    channels: SharedChannelRegistry,
    binding_id: impl Into<String>,
) {
    let def = ChannelSendTool::tool_def();
    let handler = Arc::new(ChannelSendTool::new(channels, binding_id));
    tools.register_arc(def, handler);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_mcp::channel::{ChannelRegistry, RegisteredChannel};

    fn registered(server: &str, outbound: Option<&str>) -> RegisteredChannel {
        RegisteredChannel {
            binding_id: "b".into(),
            server_name: server.into(),
            plugin_source: None,
            outbound_tool_name: outbound.map(str::to_string),
            permission_relay: false,
            registered_at_ms: 0,
        }
    }

    #[tokio::test]
    async fn lookup_returns_none_when_server_unregistered() {
        let reg: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        assert!(reg.get("b", "slack").await.is_none());
    }

    #[tokio::test]
    async fn lookup_returns_registered_entry_with_resolved_outbound() {
        let reg: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        reg.register(registered("slack", Some("chat.postMessage")))
            .await;
        let got = reg.get("b", "slack").await.unwrap();
        assert_eq!(got.outbound_tool_name.as_deref(), Some("chat.postMessage"));
    }

    #[test]
    fn tool_def_requires_server_only() {
        let def = ChannelSendTool::tool_def();
        let required = def.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "server");
    }

    #[test]
    fn tool_def_documents_arguments_passthrough() {
        let def = ChannelSendTool::tool_def();
        let props = def.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("server"));
        assert!(props.contains_key("content"));
        assert!(props.contains_key("arguments"));
    }
}
