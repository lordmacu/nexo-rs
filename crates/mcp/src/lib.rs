//! Phase 12 — MCP (Model Context Protocol) client.
//!
//! 12.1 ships the stdio transport. Later sub-phases add HTTP/SSE (12.2),
//! tool catalog (12.3), session-scoped runtime (12.4), and more.

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
// Phase 76.1 — HTTP transport top-level re-exports.
pub use server::http_config::HttpTransportConfig;
pub use server::http_transport::{start_http_server, HttpNotifyHandle, HttpServerHandle};
pub use session::{RuntimeCallError, SessionMcpRuntime};
pub use types::{
    McpAnnotations, McpCapabilities, McpClientInfo, McpContent, McpPrompt, McpPromptArgument,
    McpPromptMessage, McpPromptResult, McpResource, McpResourceContent, McpResourceRef,
    McpResourceTemplate, McpServerInfo, McpTool, McpToolResult,
};
