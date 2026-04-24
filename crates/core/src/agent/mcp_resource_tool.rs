//! Phase 12.5 — 2 meta-tools per MCP server that exposes `resources/*`:
//! `mcp_{server}_list_resources` and `mcp_{server}_read_resource`. The LLM
//! uses them to browse and pull data surfaces on demand.
use super::context::AgentContext;
use super::mcp_tool::{sanitize_name_fragment, MCP_NAME_PREFIX};
use super::tool_registry::ToolHandler;
use agent_llm::ToolDef;
use agent_mcp::{McpClient, McpResourceContent, ResourceCache};
use async_trait::async_trait;
use base64::Engine;
use serde_json::Value;
use std::sync::Arc;
/// Tail fragment used when composing the list-resources meta-tool name.
/// Kept public so downstream code can pattern-match on suffix if needed.
/// The underscore separator between server name and this fragment is
/// supplied by `ToolDef::fit_name`, not baked in here.
pub const RESOURCE_LIST_SUFFIX: &str = "list_resources";
pub const RESOURCE_READ_SUFFIX: &str = "read_resource";
pub const RESOURCE_TEMPLATES_SUFFIX: &str = "list_resource_templates";
pub struct McpResourceListTool {
    server_name: String,
    client: Arc<dyn McpClient>,
    context_passthrough: bool,
}
pub struct McpResourceReadTool {
    server_name: String,
    client: Arc<dyn McpClient>,
    context_passthrough: bool,
    cache: Option<Arc<ResourceCache>>,
    /// Phase 12.5 follow-up — if non-empty, URIs whose scheme is absent
    /// from the list are logged at `warn` and counted via
    /// `mcp_resource_uri_allowlist_violations_total`. The call still
    /// proceeds (permissive-by-design so the LLM gets an error surface
    /// from the server rather than a silent skip); operators who want
    /// hard blocking can combine this signal with a rate-limit.
    uri_allowlist: Arc<Vec<String>>,
}
pub struct McpResourceListTemplatesTool {
    server_name: String,
    client: Arc<dyn McpClient>,
}
impl McpResourceListTool {
    pub fn new(server_name: impl Into<String>, client: Arc<dyn McpClient>) -> Self {
        Self {
            server_name: server_name.into(),
            client,
            context_passthrough: false,
        }
    }
    /// Phase 12.8 — enable `_meta` propagation on `resources/list`.
    pub fn with_context_passthrough(mut self, enabled: bool) -> Self {
        self.context_passthrough = enabled;
        self
    }
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
    pub fn prefixed_name(server_name: &str) -> String {
        ToolDef::fit_name(
            MCP_NAME_PREFIX,
            &sanitize_name_fragment(server_name),
            RESOURCE_LIST_SUFFIX,
        )
    }
    pub fn tool_def(server_name: &str) -> ToolDef {
        ToolDef {
            name: Self::prefixed_name(server_name),
            description: format!(
                "[mcp:{server_name}] List data resources this server exposes. \
                 No arguments. Returns an array of {{uri, name, description, mime_type}}."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }
}
impl McpResourceReadTool {
    pub fn new(server_name: impl Into<String>, client: Arc<dyn McpClient>) -> Self {
        Self {
            server_name: server_name.into(),
            client,
            context_passthrough: false,
            cache: None,
            uri_allowlist: Arc::new(Vec::new()),
        }
    }
    pub fn with_uri_allowlist(mut self, allowlist: Arc<Vec<String>>) -> Self {
        self.uri_allowlist = allowlist;
        self
    }
    /// Phase 12.8 — enable `_meta` propagation on `resources/read`.
    pub fn with_context_passthrough(mut self, enabled: bool) -> Self {
        self.context_passthrough = enabled;
        self
    }
    /// Phase 12.5 follow-up — opt-in LRU+TTL cache shared with the owning
    /// session runtime. Cache is skipped for blob-only responses to keep
    /// memory usage bounded.
    pub fn with_cache(mut self, cache: Arc<ResourceCache>) -> Self {
        self.cache = Some(cache);
        self
    }
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
    pub fn prefixed_name(server_name: &str) -> String {
        ToolDef::fit_name(
            MCP_NAME_PREFIX,
            &sanitize_name_fragment(server_name),
            RESOURCE_READ_SUFFIX,
        )
    }
    pub fn tool_def(server_name: &str) -> ToolDef {
        ToolDef {
            name: Self::prefixed_name(server_name),
            description: format!(
                "[mcp:{server_name}] Read a data resource by uri (obtain candidate uris from \
                 the list_resources tool). Returns the resource text, or a marker for binary \
                 content."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "uri": { "type": "string" } },
                "required": ["uri"],
                "additionalProperties": false
            }),
        }
    }
    /// Collapse the raw MCP response into a single LLM-consumable string.
    pub fn flatten_read_result(contents: &[McpResourceContent]) -> Value {
        if contents.is_empty() {
            return Value::String(String::new());
        }
        let parts: Vec<String> = contents
            .iter()
            .map(|c| {
                if let Some(t) = c.text.as_deref() {
                    t.to_string()
                } else if let Some(blob) = c.blob.as_deref() {
                    let byte_len = base64::engine::general_purpose::STANDARD
                        .decode(blob)
                        .map(|b| b.len())
                        .unwrap_or(blob.len());
                    let mime = c.mime_type.as_deref().unwrap_or("?");
                    format!("[blob: {mime}, {byte_len} bytes]")
                } else {
                    "[empty resource content]".to_string()
                }
            })
            .collect();
        Value::String(parts.join("\n\n"))
    }
}
impl McpResourceListTemplatesTool {
    pub fn new(server_name: impl Into<String>, client: Arc<dyn McpClient>) -> Self {
        Self {
            server_name: server_name.into(),
            client,
        }
    }
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
    pub fn prefixed_name(server_name: &str) -> String {
        ToolDef::fit_name(
            MCP_NAME_PREFIX,
            &sanitize_name_fragment(server_name),
            RESOURCE_TEMPLATES_SUFFIX,
        )
    }
    pub fn tool_def(server_name: &str) -> ToolDef {
        ToolDef {
            name: Self::prefixed_name(server_name),
            description: format!(
                "[mcp:{server_name}] List RFC-6570 URI templates this server exposes. \
                 No arguments. Returns an array of {{uri_template, name, description, mime_type}}. \
                 Use the templates to construct concrete URIs for read_resource."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for McpResourceListTemplatesTool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        let templates = self.client.list_resource_templates().await.map_err(|e| {
            anyhow::anyhow!(
                "mcp '{}' list_resource_templates error: {e}",
                self.server_name
            )
        })?;
        let list: Vec<Value> = templates
            .into_iter()
            .map(|t| {
                let mut obj = serde_json::Map::new();
                obj.insert("uri_template".into(), Value::String(t.uri_template));
                if !t.name.is_empty() {
                    obj.insert("name".into(), Value::String(t.name));
                }
                if let Some(d) = t.description {
                    obj.insert("description".into(), Value::String(d));
                }
                if let Some(m) = t.mime_type {
                    obj.insert("mime_type".into(), Value::String(m));
                }
                Value::Object(obj)
            })
            .collect();
        Ok(Value::Array(list))
    }
}
fn make_meta(ctx: &AgentContext) -> Value {
    serde_json::json!({
        "agent_id": ctx.agent_id,
        "session_id": ctx.session_id.map(|u| u.to_string()),
    })
}
#[async_trait]
impl ToolHandler for McpResourceListTool {
    async fn call(&self, ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        let meta = if self.context_passthrough {
            Some(make_meta(ctx))
        } else {
            None
        };
        let resources = self
            .client
            .list_resources_with_meta(meta)
            .await
            .map_err(|e| anyhow::anyhow!("mcp '{}' list_resources error: {e}", self.server_name))?;
        let list: Vec<Value> = resources
            .into_iter()
            .map(|r| {
                let mut obj = serde_json::Map::new();
                obj.insert("uri".into(), Value::String(r.uri));
                if !r.name.is_empty() {
                    obj.insert("name".into(), Value::String(r.name));
                }
                if let Some(d) = r.description {
                    obj.insert("description".into(), Value::String(d));
                }
                if let Some(m) = r.mime_type {
                    obj.insert("mime_type".into(), Value::String(m));
                }
                if let Some(ann) = r.annotations {
                    let mut ann_obj = serde_json::Map::new();
                    if !ann.audience.is_empty() {
                        ann_obj.insert(
                            "audience".into(),
                            Value::Array(ann.audience.into_iter().map(Value::String).collect()),
                        );
                    }
                    if let Some(p) = ann.priority {
                        if let Some(n) = serde_json::Number::from_f64(p as f64) {
                            ann_obj.insert("priority".into(), Value::Number(n));
                        }
                    }
                    if !ann_obj.is_empty() {
                        obj.insert("annotations".into(), Value::Object(ann_obj));
                    }
                }
                Value::Object(obj)
            })
            .collect();
        Ok(Value::Array(list))
    }
}
#[async_trait]
impl ToolHandler for McpResourceReadTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let uri = args.get("uri").and_then(|v| v.as_str()).ok_or_else(|| {
            anyhow::anyhow!(
                "mcp '{}' read_resource: missing required 'uri' argument",
                self.server_name
            )
        })?;
        if !self.uri_allowlist.is_empty() {
            let scheme = uri.split(':').next().unwrap_or("");
            if !self.uri_allowlist.iter().any(|s| s == scheme) {
                tracing::warn!(
                    mcp = %self.server_name,
                    uri = %uri,
                    scheme = %scheme,
                    allowed = ?self.uri_allowlist,
                    "mcp read_resource: URI scheme outside allowlist"
                );
                agent_mcp::telemetry::inc_resource_uri_allowlist_violation(&self.server_name);
            }
        }
        // Cache is bypassed when `_meta` is propagated: the server may
        // produce user-specific content so cross-agent reuse is unsafe.
        let use_cache = self.cache.is_some() && !self.context_passthrough;
        if use_cache {
            if let Some(cached) = self
                .cache
                .as_ref()
                .and_then(|c| c.get(&self.server_name, uri))
            {
                agent_mcp::telemetry::inc_resource_cache_hit(&self.server_name);
                return Ok(McpResourceReadTool::flatten_read_result(&cached));
            }
            agent_mcp::telemetry::inc_resource_cache_miss(&self.server_name);
        }
        let meta = if self.context_passthrough {
            Some(make_meta(ctx))
        } else {
            None
        };
        let contents = self
            .client
            .read_resource_with_meta(uri, meta)
            .await
            .map_err(|e| anyhow::anyhow!("mcp '{}' read_resource error: {e}", self.server_name))?;
        if use_cache && contents.iter().any(|c| c.text.is_some()) {
            if let Some(cache) = self.cache.as_ref() {
                cache.put(&self.server_name, uri, contents.clone());
            }
        }
        Ok(McpResourceReadTool::flatten_read_result(&contents))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefixed_name_list_includes_suffix() {
        assert_eq!(
            McpResourceListTool::prefixed_name("fs"),
            "mcp_fs_list_resources"
        );
    }
    #[test]
    fn prefixed_name_read_includes_suffix() {
        assert_eq!(
            McpResourceReadTool::prefixed_name("fs@v1"),
            "mcp_fs_v1_read_resource"
        );
    }
    #[test]
    fn long_server_name_is_hashed_into_limit() {
        let server = "a".repeat(60);
        let name = McpResourceListTool::prefixed_name(&server);
        assert_eq!(name.len(), ToolDef::MAX_NAME_LEN);
        assert!(name.starts_with("mcp_"));
    }
    #[test]
    fn tool_def_description_attributed() {
        let d = McpResourceListTool::tool_def("fs");
        assert!(d.description.starts_with("[mcp:fs]"));
        assert_eq!(d.name, "mcp_fs_list_resources");
    }
    #[test]
    fn flatten_text_resource() {
        use agent_mcp::McpResourceContent;
        let c = vec![McpResourceContent {
            uri: "file:///x".into(),
            mime_type: Some("text/plain".into()),
            text: Some("hello".into()),
            blob: None,
        }];
        let v = McpResourceReadTool::flatten_read_result(&c);
        assert_eq!(v, Value::String("hello".into()));
    }
    #[test]
    fn flatten_blob_shows_mime_and_byte_count() {
        use agent_mcp::McpResourceContent;
        let c = vec![McpResourceContent {
            uri: "file:///x".into(),
            mime_type: Some("image/png".into()),
            text: None,
            blob: Some("aGVsbG8=".into()),
        }];
        let v = McpResourceReadTool::flatten_read_result(&c);
        assert_eq!(v, Value::String("[blob: image/png, 5 bytes]".into()));
    }
    #[test]
    fn flatten_empty_returns_empty_string() {
        let v = McpResourceReadTool::flatten_read_result(&[]);
        assert_eq!(v, Value::String(String::new()));
    }
    #[test]
    fn flatten_mixed_text_and_blob() {
        use agent_mcp::McpResourceContent;
        let c = vec![
            McpResourceContent {
                uri: "a".into(),
                mime_type: None,
                text: Some("summary".into()),
                blob: None,
            },
            McpResourceContent {
                uri: "b".into(),
                mime_type: Some("image/png".into()),
                text: None,
                blob: Some("aGVsbG8=".into()),
            },
        ];
        let v = McpResourceReadTool::flatten_read_result(&c);
        assert_eq!(
            v,
            Value::String("summary\n\n[blob: image/png, 5 bytes]".into())
        );
    }
}
