//! Phase 12.6 — expose this agent as an MCP server.
//!
//! The trait here is symmetric to `McpClient` (12.1) but in the opposite
//! direction: implementors receive JSON-RPC requests and produce responses.
//! `stdio::run_stdio_server` is the only transport shipped in 12.6.

pub mod stdio;

use async_trait::async_trait;
use serde_json::Value;

use crate::errors::McpError;
use crate::types::{McpServerInfo, McpTool, McpToolResult};

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
}

pub use stdio::{run_stdio_server, run_stdio_server_with_auth, run_with_io, run_with_io_auth};
