//! MCP-server-side bridge: adapt the runtime tool registry into the
//! `McpServerHandler` shape consumed by `crates/mcp` transports, plus
//! the Phase 79.M boot dispatcher that decides which runtime tools may
//! be advertised to external MCP clients.

pub mod bridge;
pub mod context;
pub mod dispatch;
pub mod telemetry;

pub use bridge::ToolRegistryBridge;
pub use context::{McpServerBootContext, McpServerBootContextBuilder};
pub use dispatch::{boot_exposable, BootResult};
