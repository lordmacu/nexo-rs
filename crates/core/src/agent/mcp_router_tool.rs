//! Phase 79.11 — `ListMcpResources` + `ReadMcpResource` LLM tools
//! that **route** across every connected MCP server in the active
//! session.
//!
//! Distinct from `mcp_resource_tool.rs` (Phase 12.5), which
//! registers ONE tool per server (`mcp__<server>__list_resources`).
//! The router-shaped tool takes the server as an argument so the
//! model has a single discovery surface — useful when an agent
//! talks to many servers and registering N×2 tools would saturate
//! the prompt budget. Prefer this tool when there are 3+ MCP
//! servers connected.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/ListMcpResourcesTool/`
//!   * `claude-code-leak/src/tools/ReadMcpResourceTool/`
//!   * `claude-code-leak/src/services/mcp/MCPConnectionManager.tsx`
//!     for the per-server lookup pattern.
//!
//! Reference (secondary):
//!   * OpenClaw `research/` — no equivalent. Single-process TS
//!     reference manages MCP through plugin-host SDK directly.
//!
//! MVP scope:
//!   * `ListMcpResources { server: Option<String>, max: Option<usize> }` —
//!     returns up to `mcp.list_max_resources` resources across the
//!     selected server (or all if `None`).
//!   * `ReadMcpResource { server, uri }` — reads one resource,
//!     capping the body at `mcp.resource_max_bytes`.
//!   * Out of scope: `McpAuth` (the `McpClient` trait does not yet
//!     expose a refresh hook; lands as a follow-up once the trait
//!     grows the method).

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};

/// Default cap on the number of resources returned by
/// `ListMcpResources`. Operators can override via the
/// (future) `mcp.list_max_resources` config knob.
pub const DEFAULT_LIST_MAX_RESOURCES: usize = 200;

/// Default cap on the bytes returned by `ReadMcpResource`.
/// Operators can override via the (future)
/// `mcp.resource_max_bytes` config knob.
pub const DEFAULT_RESOURCE_MAX_BYTES: usize = 256 * 1024;

pub struct ListMcpResourcesTool;

impl ListMcpResourcesTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "ListMcpResources".to_string(),
            description: format!(
                "List data resources exposed by connected MCP servers. Pass `server` to scope to one server, or omit to enumerate every connected server. Capped at {} entries; oversize lists set `truncated: true` so the model knows to refine the scope.",
                DEFAULT_LIST_MAX_RESOURCES
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {
                        "type": "string",
                        "description": "Server name (matches the catalog prefix, e.g. \"github\" for `mcp__github__*` tools). Omit to list across every connected server."
                    },
                    "max": {
                        "type": "integer",
                        "description": "Override the default cap on result count.",
                        "minimum": 1
                    }
                },
                "required": []
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for ListMcpResourcesTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let mcp = ctx
            .mcp
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("ListMcpResources: no MCP runtime in this context"))?;
        let server_filter = args
            .get("server")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let max = args
            .get("max")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).max(1))
            .unwrap_or(DEFAULT_LIST_MAX_RESOURCES);

        let clients = mcp.clients();
        let scoped: Vec<_> = if let Some(name) = &server_filter {
            clients
                .into_iter()
                .filter(|(server, _)| server == name)
                .collect()
        } else {
            clients
        };
        if scoped.is_empty() {
            let known: Vec<String> = mcp.clients().into_iter().map(|(n, _)| n).collect();
            if let Some(name) = &server_filter {
                return Err(anyhow::anyhow!(
                    "ListMcpResources: no MCP server named `{name}` (connected: {})",
                    known.join(", ")
                ));
            }
        }

        let mut out: Vec<Value> = Vec::new();
        let mut errors: Vec<Value> = Vec::new();
        let mut truncated = false;

        'servers: for (server_name, client) in scoped {
            match client.list_resources().await {
                Ok(resources) => {
                    for r in resources {
                        out.push(json!({
                            "server": server_name,
                            "uri": r.uri,
                            "name": r.name,
                            "description": r.description,
                            "mime_type": r.mime_type,
                        }));
                        if out.len() >= max {
                            truncated = true;
                            break 'servers;
                        }
                    }
                }
                Err(e) => {
                    errors.push(json!({
                        "server": server_name,
                        "error": e.to_string(),
                    }));
                }
            }
        }

        Ok(json!({
            "resources": out,
            "truncated": truncated,
            "errors": errors,
            "count": out.len(),
        }))
    }
}

pub struct ReadMcpResourceTool;

impl ReadMcpResourceTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "ReadMcpResource".to_string(),
            description: format!(
                "Read the body of a single MCP resource. Supply `server` (matches the catalog prefix) and `uri` (from a previous ListMcpResources call). Bodies are capped at {} bytes; oversize responses set `truncated: true` and the model should request a more specific URI.",
                DEFAULT_RESOURCE_MAX_BYTES
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {
                        "type": "string",
                        "description": "Server name (matches the catalog prefix)."
                    },
                    "uri": {
                        "type": "string",
                        "description": "Resource URI from a previous ListMcpResources entry."
                    },
                    "max_bytes": {
                        "type": "integer",
                        "description": "Override the default body cap.",
                        "minimum": 1
                    }
                },
                "required": ["server", "uri"]
            }),
        }
    }
}

fn truncate_body(text: Option<String>, max_bytes: usize) -> (Option<String>, bool) {
    match text {
        Some(s) if s.len() > max_bytes => {
            // Find the last char boundary at or before max_bytes so we
            // never split a multi-byte sequence.
            let mut end = max_bytes;
            while !s.is_char_boundary(end) && end > 0 {
                end -= 1;
            }
            (Some(s[..end].to_string()), true)
        }
        other => (other, false),
    }
}

#[async_trait]
impl ToolHandler for ReadMcpResourceTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let mcp = ctx
            .mcp
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("ReadMcpResource: no MCP runtime in this context"))?;
        let server = args
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ReadMcpResource requires `server` (string)"))?
            .to_string();
        let uri = args
            .get("uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ReadMcpResource requires `uri` (string)"))?
            .to_string();
        let max_bytes = args
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).max(1))
            .unwrap_or(DEFAULT_RESOURCE_MAX_BYTES);

        let client = mcp
            .clients()
            .into_iter()
            .find(|(name, _)| name == &server)
            .map(|(_, c)| c)
            .ok_or_else(|| {
                let known: Vec<String> =
                    mcp.clients().into_iter().map(|(n, _)| n).collect();
                anyhow::anyhow!(
                    "ReadMcpResource: no MCP server named `{server}` (connected: {})",
                    known.join(", ")
                )
            })?;

        let contents = client
            .read_resource(&uri)
            .await
            .map_err(|e| anyhow::anyhow!("ReadMcpResource: {e}"))?;

        let mut out_contents: Vec<Value> = Vec::with_capacity(contents.len());
        let mut any_truncated = false;
        for c in contents {
            let (text, t1) = truncate_body(c.text, max_bytes);
            // Blob bodies are base64-encoded — we report length but
            // do not attempt to truncate (truncating base64 mid-string
            // breaks decoding).
            let blob_len = c.blob.as_ref().map(|b| b.len());
            any_truncated |= t1;
            out_contents.push(json!({
                "uri": c.uri,
                "mime_type": c.mime_type,
                "text": text,
                "blob_length": blob_len,
                "blob": c.blob,
            }));
        }

        Ok(json!({
            "server": server,
            "uri": uri,
            "contents": out_contents,
            "truncated": any_truncated,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_body_short_passthrough() {
        let (out, t) = truncate_body(Some("hello".to_string()), 100);
        assert_eq!(out.as_deref(), Some("hello"));
        assert!(!t);
    }

    #[test]
    fn truncate_body_long_truncates() {
        let big = "x".repeat(500);
        let (out, t) = truncate_body(Some(big.clone()), 100);
        assert_eq!(out.as_deref().unwrap().len(), 100);
        assert!(t);
    }

    #[test]
    fn truncate_body_respects_char_boundary() {
        // Each emoji is 4 bytes in UTF-8.
        let s = "🚀".repeat(50); // 200 bytes
        let (out, t) = truncate_body(Some(s.clone()), 10);
        assert!(t);
        let truncated = out.unwrap();
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(truncated.len() <= 10);
    }

    #[test]
    fn truncate_body_none_passthrough() {
        let (out, t) = truncate_body(None, 100);
        assert!(out.is_none());
        assert!(!t);
    }

    #[test]
    fn list_tool_requires_mcp_runtime() {
        // Without ctx.mcp set, the tool errors clearly.
        // Smoke test that the error path triggers — we don't go through
        // the full async runtime here, just check the static tool_def
        // returns a sane shape.
        let def = ListMcpResourcesTool::tool_def();
        assert_eq!(def.name, "ListMcpResources");
        let props = def.parameters.get("properties").unwrap();
        assert!(props.get("server").is_some());
        assert!(props.get("max").is_some());
        // `server` is optional.
        let required = def.parameters.get("required").unwrap().as_array().unwrap();
        assert!(required.is_empty());
    }

    #[test]
    fn read_tool_def_required_fields() {
        let def = ReadMcpResourceTool::tool_def();
        assert_eq!(def.name, "ReadMcpResource");
        let required: Vec<&str> = def
            .parameters
            .get("required")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(required.contains(&"server"));
        assert!(required.contains(&"uri"));
        assert!(!required.contains(&"max_bytes"));
    }
}
