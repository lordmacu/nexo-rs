//! Phase 12.2 — unified trait over stdio and HTTP clients.
//!
//! `StdioMcpClient` (12.1) and `HttpMcpClient` (12.2) both implement this
//! trait so `SessionMcpRuntime`, `McpToolCatalog`, and the LLM tool
//! registry can handle them interchangeably.

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::client::McpClientState;
use crate::errors::McpError;
use crate::events::ClientEvent;
use crate::types::{
    McpCapabilities, McpResource, McpResourceContent, McpServerInfo, McpTool, McpToolResult,
};

#[async_trait]
pub trait McpClient: Send + Sync {
    fn name(&self) -> &str;
    fn server_info(&self) -> &McpServerInfo;
    fn capabilities(&self) -> &McpCapabilities;
    fn protocol_version(&self) -> &str;
    fn state(&self) -> McpClientState;

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError>;
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError>;

    /// Phase 12.8 — variant that threads an optional `_meta` object
    /// alongside `arguments` in the `tools/call` request params (MCP
    /// spec 2024-11-05). Default delegates to `call_tool` so existing
    /// mocks and third-party impls keep working without changes.
    async fn call_tool_with_meta(
        &self,
        name: &str,
        arguments: Value,
        _meta: Option<Value>,
    ) -> Result<McpToolResult, McpError> {
        self.call_tool(name, arguments).await
    }

    async fn shutdown(&self);

    /// Phase 12.5 — list data resources exposed by this server. Default
    /// returns `Protocol` error; stdio/http impls override when connected.
    async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
        Err(McpError::Protocol(
            "resources not supported by this client".into(),
        ))
    }

    /// Phase 12.5 — read the resource identified by `uri`. Default returns
    /// `Protocol` error; stdio/http impls override when connected.
    async fn read_resource(&self, _uri: &str) -> Result<Vec<McpResourceContent>, McpError> {
        Err(McpError::Protocol(
            "resources not supported by this client".into(),
        ))
    }

    /// Phase 12.8 — `resources/list` with optional `_meta`. Default
    /// delegates to `list_resources` and ignores the meta for back-compat
    /// with third-party mocks.
    async fn list_resources_with_meta(
        &self,
        _meta: Option<Value>,
    ) -> Result<Vec<McpResource>, McpError> {
        self.list_resources().await
    }

    /// Phase 12.8 — `resources/read` with optional `_meta`.
    async fn read_resource_with_meta(
        &self,
        uri: &str,
        _meta: Option<Value>,
    ) -> Result<Vec<McpResourceContent>, McpError> {
        self.read_resource(uri).await
    }

    /// Phase 12.8 — `resources/templates/list`. Default returns
    /// `Protocol` error; stdio/http impls override when connected.
    async fn list_resource_templates(
        &self,
    ) -> Result<Vec<crate::types::McpResourceTemplate>, McpError> {
        Err(McpError::Protocol(
            "resource templates not supported by this client".into(),
        ))
    }

    /// Phase 12.8 — subscribe to server-pushed notifications (tools/list
    /// changed, resources/list changed, etc.). Default returns a closed
    /// receiver so clients without event support never fire.
    fn subscribe_events(&self) -> broadcast::Receiver<ClientEvent> {
        let (tx, rx) = broadcast::channel(1);
        drop(tx);
        rx
    }

    /// Phase 12.8 — like `shutdown`, but thread a human-readable `reason`
    /// into every `notifications/cancelled` payload the transport emits
    /// before tearing down. Default delegates to `shutdown()`; stdio/http
    /// overrides implement the MCP-spec-compliant behaviour.
    async fn shutdown_with_reason(&self, _reason: &str) {
        self.shutdown().await;
    }

    /// Phase 12.8 — send MCP `logging/setLevel`. Default returns a
    /// `Protocol` error so callers can tell "not implemented" from
    /// transport failure. Stdio/HTTP impls override.
    async fn set_log_level(&self, _level: &str) -> Result<(), McpError> {
        Err(McpError::Protocol(
            "logging/setLevel not supported by this client".into(),
        ))
    }
}

/// Log levels defined by the MCP 2024-11-05 spec (RFC 5424 subset).
pub const MCP_LOG_LEVELS: &[&str] = &[
    "debug",
    "info",
    "notice",
    "warning",
    "error",
    "critical",
    "alert",
    "emergency",
];
