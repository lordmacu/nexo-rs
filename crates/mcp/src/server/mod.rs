//! Phase 12.6 — expose this agent as an MCP server.
//!
//! The trait here is symmetric to `McpClient` (12.1) but in the
//! opposite direction: implementors receive JSON-RPC requests and
//! produce responses.
//!
//! Phase 76.2 — internal split:
//!   * `transport` — `McpTransport` trait + `Frame` / `TransportError`.
//!   * `dispatch` — `Dispatcher`, `DispatchContext`, `DispatchOutcome`
//!     (transport-agnostic JSON-RPC handling, panic-safe, cancel-aware).
//!   * `stdio_transport` — `StdioTransport` impl + the generic
//!     `server_loop` driver.
//!   * `stdio` — preserved public API (`run_stdio_server`, …) which
//!     now delegates to the pieces above.

pub mod audit_log;
pub mod auth;
pub mod dispatch;
pub mod http_config;
pub mod http_session;
pub mod per_principal_concurrency;
pub mod per_principal_rate_limit;
pub mod progress;
pub mod telemetry;
// Phase 76.7 — promoted to `pub` so external integration tests
// can import `SessionEvent` for asserting on broadcast traffic.
// (Originally `pub(crate)` from Phase 76.1.)
pub mod http_transport;
pub mod parse;
pub mod stdio;
pub(crate) mod stdio_transport;
pub mod transport;

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

    /// Flat capability flags. Phase 76.7 — the in-tree dispatcher
    /// supports `notifications/tools/list_changed`,
    /// `notifications/resources/list_changed`,
    /// `notifications/resources/updated` (via
    /// `HttpServerHandle::notify_*`) and `resources/subscribe`,
    /// so we advertise `listChanged: true` + `subscribe: true`
    /// by default. Implementors that want to disable subscription
    /// support can override this method.
    fn capabilities(&self) -> Value {
        serde_json::json!({
            "tools": { "listChanged": true },
            "resources": { "listChanged": true, "subscribe": true },
        })
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError>;

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError>;

    /// Phase 76.7 — streaming-aware variant. Default delegates
    /// to [`call_tool`] and ignores the reporter, so existing
    /// handlers compile unchanged. Tools that want to emit
    /// `notifications/progress` override this method and call
    /// `progress.report(..)` periodically.
    ///
    /// The `progress` reporter is a noop when the originating
    /// request did not include `params._meta.progressToken`,
    /// so handlers can call `report` unconditionally without
    /// branching.
    async fn call_tool_streaming(
        &self,
        name: &str,
        arguments: Value,
        progress: progress::ProgressReporter,
    ) -> Result<McpToolResult, McpError> {
        let _ = progress;
        self.call_tool(name, arguments).await
    }

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

pub use auth::{
    AuthConfig, AuthMethod, AuthRejection, AuthRejectionReason, McpAuthenticator, Principal,
};
pub use dispatch::{DispatchContext, DispatchOutcome, Dispatcher};
pub use stdio::{run_stdio_server, run_stdio_server_with_auth, run_with_io, run_with_io_auth};
pub use transport::{Frame, McpTransport, TransportError};
