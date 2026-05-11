//! MCP (Model Context Protocol) client.
//!
//! Provides the stdio transport, HTTP/SSE transport, tool catalog,
//! and a session-scoped runtime.

pub mod channel;
pub mod channel_boot;
pub mod channel_bridge;
pub mod channel_permission;
pub mod channel_session_store;
pub mod client;
pub mod client_trait;
pub mod config;
pub mod config_watch;
pub mod errors;
pub mod events;
pub mod http;
pub mod logging;
pub mod manager;
pub mod protocol;
pub mod resource_cache;
pub mod runtime_config;
pub mod sampling;
pub mod server;
pub mod session;
pub mod telemetry;
pub mod types;

pub use client::{McpClientState, StdioMcpClient};
pub use client_trait::McpClient;
pub use config::McpServerConfig;
pub use config_watch::{spawn_mcp_config_watcher, MCP_YAML_FILENAME};
pub use errors::McpError;
pub use events::{method_to_event, ClientEvent};
pub use http::{HttpMcpClient, HttpMcpOptions, HttpTransportMode};
pub use manager::McpRuntimeManager;
pub use resource_cache::{ResourceCache, ResourceCacheConfig};
pub use runtime_config::{McpRuntimeConfig, McpServerRuntimeConfig};
pub use server::{
    run_stdio_server, run_stdio_server_with_auth, run_with_io, run_with_io_auth, McpServerHandler,
};
// HTTP transport top-level re-exports.
pub use server::http_config::HttpTransportConfig;
pub use server::http_transport::{start_http_server, HttpNotifyHandle, HttpServerHandle};
pub use session::{RuntimeCallError, SessionMcpRuntime};
pub use types::{
    McpAnnotations, McpCapabilities, McpClientInfo, McpContent, McpPrompt, McpPromptArgument,
    McpPromptMessage, McpPromptResult, McpResource, McpResourceContent, McpResourceRef,
    McpResourceTemplate, McpServerInfo, McpTool, McpToolResult,
};
