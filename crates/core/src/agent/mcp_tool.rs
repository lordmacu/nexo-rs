//! Phase 12.3 — bridge an MCP server's tools into the agent's `ToolRegistry`.
//!
//! Parallel to `extension_tool.rs` (11.5). The LLM sees tools prefixed with
//! `mcp_` and attributed with `[mcp:<server>]` in the description; calls
//! are routed to the owning `dyn McpClient`.
use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use nexo_llm::ToolDef;
use nexo_mcp::{McpClient, McpContent, McpTool as McpToolDescriptor};
use async_trait::async_trait;
use base64::Engine;
use serde_json::Value;
use std::sync::Arc;
pub const MCP_NAME_PREFIX: &str = "mcp_";
/// Replace every character outside `[a-zA-Z0-9_-]` with `_`. Used on both
/// server and tool name before building the prefixed LLM-facing name so no
/// LLM provider rejects the identifier.
pub fn sanitize_name_fragment(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
pub struct McpTool {
    server_name: String,
    tool_name: String,
    client: Arc<dyn McpClient>,
    context_passthrough: bool,
}
impl McpTool {
    pub fn new(
        server_name: impl Into<String>,
        tool_name: impl Into<String>,
        client: Arc<dyn McpClient>,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            tool_name: tool_name.into(),
            client,
            context_passthrough: false,
        }
    }
    /// Phase 12.8 — builder that enables `_meta` propagation (agent_id,
    /// session_id) on every `tools/call` this handler emits.
    pub fn with_context_passthrough(mut self, enabled: bool) -> Self {
        self.context_passthrough = enabled;
        self
    }
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }
    /// Full LLM-facing tool name: `mcp_{server}_{tool}` with both segments
    /// passed through `sanitize_name_fragment`. Overflow falls back to a
    /// deterministic hash suffix via `ToolDef::fit_name`.
    pub fn prefixed_name(server_name: &str, tool_name: &str) -> String {
        ToolDef::fit_name(
            MCP_NAME_PREFIX,
            &sanitize_name_fragment(server_name),
            &sanitize_name_fragment(tool_name),
        )
    }
    /// Build a `ToolDef` ready for `ToolRegistry::register`. Description is
    /// decorated with `[mcp:<server>]` so the LLM knows where the tool is
    /// from when reasoning over its options.
    pub fn tool_def(desc: &McpToolDescriptor, server_name: &str) -> ToolDef {
        let description = match desc.description.as_deref() {
            Some(d) => format!("[mcp:{server_name}] {d}"),
            None => format!("[mcp:{server_name}] "),
        };
        ToolDef {
            name: Self::prefixed_name(server_name, &desc.name),
            description,
            parameters: desc.input_schema.clone(),
        }
    }
    /// Flatten the MCP `content` array into a JSON string the LLM can consume
    /// directly. Non-text blocks are represented as inline markers so the LLM
    /// still knows something was returned.
    pub fn flatten_content(content: &[McpContent]) -> Value {
        let mut parts: Vec<String> = Vec::with_capacity(content.len());
        for c in content {
            match c {
                McpContent::Text { text } => parts.push(text.clone()),
                McpContent::Image { data, mime_type } => {
                    let byte_len = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .map(|b| b.len())
                        .unwrap_or_else(|_| data.len());
                    parts.push(format!("[image: {mime_type}, {byte_len} bytes]"));
                }
                McpContent::Resource { resource } => {
                    if let Some(t) = resource.text.as_deref() {
                        parts.push(t.to_string());
                    } else {
                        parts.push(format!("[resource: {}]", resource.uri));
                    }
                }
            }
        }
        Value::String(parts.join("\n\n"))
    }
}
#[async_trait]
impl ToolHandler for McpTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let meta = if self.context_passthrough {
            Some(serde_json::json!({
                "agent_id": ctx.agent_id,
                "session_id": ctx.session_id.map(|u| u.to_string()),
            }))
        } else {
            None
        };
        match self
            .client
            .call_tool_with_meta(&self.tool_name, args, meta)
            .await
        {
            Ok(result) => {
                let flattened = McpTool::flatten_content(&result.content);
                if result.is_error {
                    let text = flattened.as_str().unwrap_or("");
                    Err(anyhow::anyhow!(
                        "mcp '{}' tool '{}' failed: {}",
                        self.server_name,
                        self.tool_name,
                        text
                    ))
                } else {
                    Ok(flattened)
                }
            }
            Err(e) => Err(anyhow::anyhow!(
                "mcp '{}' tool '{}' error: {e}",
                self.server_name,
                self.tool_name
            )),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use nexo_mcp::McpResourceRef;
    #[test]
    fn sanitize_passes_alphanumeric_hyphen_underscore() {
        assert_eq!(sanitize_name_fragment("abc_123-xyz"), "abc_123-xyz");
    }
    #[test]
    fn sanitize_replaces_at_sign() {
        assert_eq!(sanitize_name_fragment("fs@v1"), "fs_v1");
    }
    #[test]
    fn sanitize_replaces_spaces_and_dots() {
        assert_eq!(sanitize_name_fragment("my server.v2"), "my_server_v2");
    }
    #[test]
    fn sanitize_empty_stays_empty() {
        assert_eq!(sanitize_name_fragment(""), "");
    }
    #[test]
    fn prefixed_name_concatenates_sanitized() {
        assert_eq!(
            McpTool::prefixed_name("fs@v1", "read.file"),
            "mcp_fs_v1_read_file"
        );
    }
    #[test]
    fn prefixed_name_at_exactly_max_is_unchanged() {
        let s = "a".repeat(28);
        let t = "b".repeat(31);
        assert_eq!(McpTool::prefixed_name(&s, &t).len(), 64);
    }
    #[test]
    fn long_mcp_name_is_hashed_into_limit() {
        let s = "a".repeat(28);
        let t = "b".repeat(60);
        let name = McpTool::prefixed_name(&s, &t);
        assert_eq!(name.len(), 64);
        assert!(name.starts_with("mcp_"));
    }
    #[test]
    fn different_long_mcp_tools_hash_distinct() {
        let s = "srv";
        let a = McpTool::prefixed_name(s, &"aaaa_".repeat(20));
        let b = McpTool::prefixed_name(s, &"bbbb_".repeat(20));
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);
    }
    #[test]
    fn tool_def_decorates_description_present() {
        let desc = McpToolDescriptor {
            name: "echo".into(),
            description: Some("echoes".into()),
            input_schema: serde_json::json!({"type":"object"}),
        };
        let def = McpTool::tool_def(&desc, "util");
        assert_eq!(def.name, "mcp_util_echo");
        assert_eq!(def.description, "[mcp:util] echoes");
        assert_eq!(def.parameters, serde_json::json!({"type":"object"}));
    }
    #[test]
    fn tool_def_handles_missing_description() {
        let desc = McpToolDescriptor {
            name: "ping".into(),
            description: None,
            input_schema: serde_json::json!({}),
        };
        let def = McpTool::tool_def(&desc, "util");
        assert!(def.description.starts_with("[mcp:util]"));
    }
    #[test]
    fn flatten_single_text() {
        let v = McpTool::flatten_content(&[McpContent::Text {
            text: "hello".into(),
        }]);
        assert_eq!(v, Value::String("hello".into()));
    }
    #[test]
    fn flatten_multi_text_joins_with_blank_line() {
        let v = McpTool::flatten_content(&[
            McpContent::Text { text: "a".into() },
            McpContent::Text { text: "b".into() },
        ]);
        assert_eq!(v, Value::String("a\n\nb".into()));
    }
    #[test]
    fn flatten_image_shows_mime_and_byte_count() {
        // "aGVsbG8=" decodes to "hello" (5 bytes)
        let v = McpTool::flatten_content(&[McpContent::Image {
            data: "aGVsbG8=".into(),
            mime_type: "image/png".into(),
        }]);
        assert_eq!(v, Value::String("[image: image/png, 5 bytes]".into()));
    }
    #[test]
    fn flatten_resource_with_text_returns_text() {
        let v = McpTool::flatten_content(&[McpContent::Resource {
            resource: McpResourceRef {
                uri: "file:///x".into(),
                text: Some("body".into()),
                mime_type: None,
            },
        }]);
        assert_eq!(v, Value::String("body".into()));
    }
    #[test]
    fn flatten_resource_without_text_uses_uri_marker() {
        let v = McpTool::flatten_content(&[McpContent::Resource {
            resource: McpResourceRef {
                uri: "file:///x".into(),
                text: None,
                mime_type: None,
            },
        }]);
        assert_eq!(v, Value::String("[resource: file:///x]".into()));
    }
    #[test]
    fn flatten_mixed_content() {
        let v = McpTool::flatten_content(&[
            McpContent::Text {
                text: "summary".into(),
            },
            McpContent::Image {
                data: "aGVsbG8=".into(),
                mime_type: "image/png".into(),
            },
            McpContent::Resource {
                resource: McpResourceRef {
                    uri: "file:///r".into(),
                    text: Some("body".into()),
                    mime_type: None,
                },
            },
        ]);
        assert_eq!(
            v,
            Value::String("summary\n\n[image: image/png, 5 bytes]\n\nbody".into())
        );
    }
    #[test]
    fn flatten_empty_returns_empty_string() {
        let v = McpTool::flatten_content(&[]);
        assert_eq!(v, Value::String(String::new()));
    }
}
