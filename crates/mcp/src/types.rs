//! Domain types exposed by MCP servers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpServerInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// Server-declared capabilities flattened to booleans. The upstream spec has
/// nested objects with `listChanged` / `subscribe` flags; 12.1 only cares
/// whether a feature is present at all.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpCapabilities {
    #[serde(default, deserialize_with = "de_feature_flag")]
    pub tools: bool,
    #[serde(default, deserialize_with = "de_feature_flag")]
    pub resources: bool,
    #[serde(default, deserialize_with = "de_feature_flag")]
    pub prompts: bool,
    #[serde(default, deserialize_with = "de_feature_flag")]
    pub logging: bool,
}

fn de_feature_flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    Ok(match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => b,
        // Any object `{...}` or non-empty value means "feature present".
        serde_json::Value::Object(_) => true,
        _ => false,
    })
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: serde_json::Value,
    /// Phase 74.2 — MCP 2025-11-25 SEP-986. Optional JSON Schema
    /// describing the **success** payload of `tools/call`. When
    /// present, the tool's `McpToolResult.structured_content`
    /// (Phase 74.3) MUST validate against this schema. Claude
    /// Code 2.1 uses this to type-check responses before forwarding
    /// to the model — declaring it explicitly closes the class of
    /// "schema drift" bugs that hit Phase 73's `updatedInput` flap.
    /// Servers that don't have a stable response shape can leave
    /// it as `None` (default); the field is omitted from the wire
    /// then, matching pre-2025-11-25 behaviour.
    #[serde(
        default,
        rename = "outputSchema",
        skip_serializing_if = "Option::is_none"
    )]
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Resource {
        resource: McpResourceRef,
    },
}

impl Default for McpContent {
    fn default() -> Self {
        McpContent::Text {
            text: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpResourceRef {
    pub uri: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct McpToolResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
    /// Phase 74.3 — MCP 2025-11-25 typed result. When `Some`,
    /// Claude Code 2.1 prefers this over re-parsing
    /// `content[0].text` as JSON, which is the round-trip that
    /// surfaced the Phase 73 `updatedInput` validation flap (the
    /// text was re-serialised through Claude's Zod schema and the
    /// drift between our shape and theirs blew up). Servers that
    /// declare an `outputSchema` on the tool SHOULD populate this
    /// so the validator runs against the typed object directly.
    /// Omitted from the wire when `None` to stay compatible with
    /// pre-2025-11-25 clients.
    #[serde(
        default,
        rename = "structuredContent",
        skip_serializing_if = "Option::is_none"
    )]
    pub structured_content: Option<serde_json::Value>,
}

/// Server handshake response carried on `initialize`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct InitializeResult {
    #[serde(default, rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: McpCapabilities,
    #[serde(default, rename = "serverInfo")]
    pub server_info: McpServerInfo,
}

/// Paginated `tools/list` envelope.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ToolsListPage {
    #[serde(default)]
    pub tools: Vec<McpTool>,
    #[serde(default, rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// Phase 12.5 — MCP data resource declaration as returned by `resources/list`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct McpResource {
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    /// Phase 12.5 follow-up — optional metadata the server uses to hint
    /// the intended audience (`"user"`/`"assistant"`) and relative
    /// priority (0.0–1.0). LLM-facing tools surface this so the model can
    /// triage which resources to pull first.
    #[serde(default)]
    pub annotations: Option<McpAnnotations>,
}

/// Phase 12.5 follow-up — MCP resource annotations. Free-form object in
/// the spec; we decode the two known fields and ignore the rest.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct McpAnnotations {
    /// Non-empty when the server scopes this resource to specific
    /// roles; typical values: `["user"]`, `["assistant"]`, or both.
    #[serde(default)]
    pub audience: Vec<String>,
    /// Relative priority 0.0–1.0 suggested by the server. `None` when
    /// the server does not include it.
    #[serde(default)]
    pub priority: Option<f32>,
}

/// Body of each element inside `resources/read` `contents` array.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpResourceContent {
    pub uri: String,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    /// Base64-encoded binary body when the resource is not plain text.
    #[serde(default)]
    pub blob: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResourcesListPage {
    #[serde(default)]
    pub resources: Vec<McpResource>,
    #[serde(default, rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// Phase 12.8 — RFC-6570 URI template exposed by an MCP server.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpResourceTemplate {
    #[serde(default, rename = "uriTemplate")]
    pub uri_template: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ResourceTemplatesPage {
    #[serde(default, rename = "resourceTemplates")]
    pub resources: Vec<McpResourceTemplate>,
    #[serde(default, rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResourceReadResult {
    #[serde(default)]
    pub contents: Vec<McpResourceContent>,
}

/// Prompt declaration returned by `prompts/list`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpPrompt {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

/// Prompt argument schema field. Kept intentionally small: enough for
/// clients to render the prompt picker without requiring full JSON Schema.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpPromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

/// Result envelope returned by `prompts/get`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct McpPromptResult {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub messages: Vec<McpPromptMessage>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct McpPromptMessage {
    #[serde(default)]
    pub role: String,
    pub content: McpContent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_content_deserializes() {
        let j = serde_json::json!({"type":"text","text":"hi"});
        let c: McpContent = serde_json::from_value(j).unwrap();
        assert_eq!(c, McpContent::Text { text: "hi".into() });
    }

    #[test]
    fn image_content_deserializes() {
        let j = serde_json::json!({"type":"image","data":"aGVsbG8=","mimeType":"image/png"});
        let c: McpContent = serde_json::from_value(j).unwrap();
        assert_eq!(
            c,
            McpContent::Image {
                data: "aGVsbG8=".into(),
                mime_type: "image/png".into()
            }
        );
    }

    #[test]
    fn resource_content_deserializes() {
        let j = serde_json::json!({
            "type":"resource",
            "resource":{"uri":"file:///x","text":"body","mimeType":"text/plain"}
        });
        let c: McpContent = serde_json::from_value(j).unwrap();
        if let McpContent::Resource { resource } = c {
            assert_eq!(resource.uri, "file:///x");
            assert_eq!(resource.text.as_deref(), Some("body"));
            assert_eq!(resource.mime_type.as_deref(), Some("text/plain"));
        } else {
            panic!("expected Resource");
        }
    }

    #[test]
    fn tool_deserializes_with_input_schema() {
        let j = serde_json::json!({
            "name":"read_file",
            "description":"read a file",
            "inputSchema":{"type":"object","properties":{"path":{"type":"string"}}}
        });
        let t: McpTool = serde_json::from_value(j).unwrap();
        assert_eq!(t.name, "read_file");
        assert!(t.input_schema.is_object());
    }

    #[test]
    fn capabilities_feature_flag_accepts_object_or_bool() {
        let j = serde_json::json!({"tools":{"listChanged":true},"resources":{},"logging":{}});
        let c: McpCapabilities = serde_json::from_value(j).unwrap();
        assert!(c.tools);
        assert!(c.resources);
        assert!(c.logging);
        assert!(!c.prompts);
    }

    #[test]
    fn resource_min_deserialize() {
        let j = serde_json::json!({"uri": "file:///x"});
        let r: McpResource = serde_json::from_value(j).unwrap();
        assert_eq!(r.uri, "file:///x");
        assert_eq!(r.name, "");
        assert!(r.description.is_none());
        assert!(r.mime_type.is_none());
    }

    #[test]
    fn resource_full_deserialize() {
        let j = serde_json::json!({
            "uri": "file:///readme",
            "name": "readme",
            "description": "top-level",
            "mimeType": "text/plain"
        });
        let r: McpResource = serde_json::from_value(j).unwrap();
        assert_eq!(r.uri, "file:///readme");
        assert_eq!(r.name, "readme");
        assert_eq!(r.description.as_deref(), Some("top-level"));
        assert_eq!(r.mime_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn resource_annotations_deserialize() {
        let j = serde_json::json!({
            "uri": "file:///x",
            "annotations": {"audience":["user","assistant"],"priority":0.5}
        });
        let r: McpResource = serde_json::from_value(j).unwrap();
        let ann = r.annotations.expect("annotations");
        assert_eq!(
            ann.audience,
            vec!["user".to_string(), "assistant".to_string()]
        );
        assert_eq!(ann.priority, Some(0.5));
    }

    #[test]
    fn resource_annotations_absent_ok() {
        let j = serde_json::json!({ "uri": "file:///x" });
        let r: McpResource = serde_json::from_value(j).unwrap();
        assert!(r.annotations.is_none());
    }

    #[test]
    fn resource_content_text_variant() {
        let j = serde_json::json!({
            "uri": "file:///readme",
            "text": "hello",
            "mimeType": "text/plain"
        });
        let c: McpResourceContent = serde_json::from_value(j).unwrap();
        assert_eq!(c.text.as_deref(), Some("hello"));
        assert!(c.blob.is_none());
    }

    #[test]
    fn resource_content_blob_variant() {
        let j = serde_json::json!({
            "uri": "file:///blob",
            "blob": "aGVsbG8=",
            "mimeType": "image/png"
        });
        let c: McpResourceContent = serde_json::from_value(j).unwrap();
        assert!(c.text.is_none());
        assert_eq!(c.blob.as_deref(), Some("aGVsbG8="));
    }

    #[test]
    fn tool_result_is_error_flag() {
        let j = serde_json::json!({"content":[{"type":"text","text":"nope"}],"isError":true});
        let r: McpToolResult = serde_json::from_value(j).unwrap();
        assert!(r.is_error);
        assert_eq!(r.content.len(), 1);
    }
}
