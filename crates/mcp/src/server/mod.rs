//! Phase 12.6 — expose this agent as an MCP server.
//!
//! The trait here is symmetric to `McpClient` (12.1) but in the opposite
//! direction: implementors receive JSON-RPC requests and produce responses.
//! `stdio::run_stdio_server` is the only transport shipped in 12.6.

pub mod stdio;

use async_trait::async_trait;
use serde_json::Value;

use crate::errors::McpError;
use crate::types::{
    McpPrompt, McpPromptResult, McpResource, McpResourceContent, McpResourceTemplate,
    McpServerInfo, McpTool, McpToolResult,
};

#[async_trait]
pub trait McpServerHandler: Send + Sync {
    /// Published as `serverInfo` in the `initialize` response.
    fn server_info(&self) -> McpServerInfo;

    /// Flat capability flags. Default advertises tools only; implementors
    /// that surface resources/prompts/logging return a richer object.
    fn capabilities(&self) -> Value {
        serde_json::json!({ "tools": { "listChanged": false } })
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError>;

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError>;

    /// Optional `resources/list` support.
    async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
        Err(McpError::Protocol(
            "resources not supported by this server".into(),
        ))
    }

    /// Optional `resources/read` support.
    async fn read_resource(&self, _uri: &str) -> Result<Vec<McpResourceContent>, McpError> {
        Err(McpError::Protocol(
            "resources not supported by this server".into(),
        ))
    }

    /// Optional `resources/templates/list` support.
    async fn list_resource_templates(&self) -> Result<Vec<McpResourceTemplate>, McpError> {
        Err(McpError::Protocol(
            "resource templates not supported by this server".into(),
        ))
    }

    /// Optional `prompts/list` support.
    async fn list_prompts(&self) -> Result<Vec<McpPrompt>, McpError> {
        Err(McpError::Protocol(
            "prompts not supported by this server".into(),
        ))
    }

    /// Optional `prompts/get` support.
    async fn get_prompt(
        &self,
        _name: &str,
        _arguments: Value,
    ) -> Result<McpPromptResult, McpError> {
        Err(McpError::Protocol(
            "prompts not supported by this server".into(),
        ))
    }
}

pub use stdio::{run_stdio_server, run_stdio_server_with_auth, run_with_io, run_with_io_auth};
