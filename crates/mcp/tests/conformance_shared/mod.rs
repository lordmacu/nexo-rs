//! Shared conformance handler used by both `http_conformance_test`
//! and `stdio_conformance_test` (Phase 76.12).

use async_trait::async_trait;
use nexo_mcp::types::{
    McpContent, McpPrompt, McpPromptMessage, McpPromptResult, McpResource, McpResourceContent,
    McpServerInfo, McpTool, McpToolResult,
};
use nexo_mcp::{McpError, McpServerHandler};
use serde_json::Value;

#[derive(Clone)]
pub struct ConformanceHandler;

#[async_trait]
impl McpServerHandler for ConformanceHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "mock".into(),
            version: "0.1.0".into(),
        }
    }
    fn capabilities(&self) -> Value {
        serde_json::json!({
            "tools": { "listChanged": false },
            "resources": {},
            "prompts": {},
        })
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "ping".into(),
            description: Some("x".into()),
            input_schema: serde_json::json!({"type":"object"}),
            output_schema: None,
        }])
    }
    async fn call_tool(&self, name: &str, _: Value) -> Result<McpToolResult, McpError> {
        if name == "ghost" {
            return Err(McpError::Protocol("tool not found".into()));
        }
        Ok(McpToolResult {
            content: vec![McpContent::Text {
                text: format!("called {name}"),
            }],
            is_error: false,
            structured_content: None,
        })
    }
    async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
        Ok(vec![McpResource {
            uri: "agent://workspace/soul".into(),
            name: "SOUL.md".into(),
            description: Some("resource".into()),
            mime_type: Some("text/plain".into()),
            annotations: None,
        }])
    }
    async fn read_resource(&self, uri: &str) -> Result<Vec<McpResourceContent>, McpError> {
        Ok(vec![McpResourceContent {
            uri: uri.into(),
            mime_type: Some("text/plain".into()),
            text: Some(format!("contents {uri}")),
            blob: None,
        }])
    }
    async fn list_prompts(&self) -> Result<Vec<McpPrompt>, McpError> {
        Ok(vec![McpPrompt {
            name: "workspace_soul".into(),
            description: Some("prompt".into()),
            arguments: vec![],
        }])
    }
    async fn get_prompt(&self, name: &str, _: Value) -> Result<McpPromptResult, McpError> {
        Ok(McpPromptResult {
            description: Some(format!("prompt {name}")),
            messages: vec![McpPromptMessage {
                role: "user".into(),
                content: McpContent::Text {
                    text: format!("run {name}"),
                },
            }],
        })
    }
}
