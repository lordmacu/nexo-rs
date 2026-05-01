//! `Microapp` builder — the public entry point.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};

use crate::errors::Result as SdkResult;
use crate::hook::HookHandler;
use crate::runtime::{dispatch_loop, Handlers, ToolHandler};

/// Type alias for the registry of `(tool_name → handler)` and
/// `(hook_name → handler)` pairs the builder accumulates. Public
/// only so the test harness (feature `test-harness`) can reach
/// in.
pub type HandlerRegistry = Handlers;

type InitCallback = Pin<Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = SdkResult<()>> + Send>> + Send>>;
type ShutdownCallback = Pin<Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>>;

/// Builder for a Phase 11 stdio microapp.
///
/// Chained API — each `with_*` consumes `self` and returns the
/// updated builder. Final `run_stdio` (or `run_with`) consumes
/// the builder and runs the JSON-RPC dispatch loop.
pub struct Microapp {
    name: String,
    version: String,
    tools: BTreeMap<String, Arc<dyn ToolHandler>>,
    hooks: BTreeMap<String, Arc<dyn HookHandler>>,
    on_initialize: Option<InitCallback>,
    on_shutdown: Option<ShutdownCallback>,
}

impl Microapp {
    /// Build a fresh microapp with the given identity.
    ///
    /// `name` is the value the daemon sees in `initialize.server_info.name`;
    /// `version` is typically `env!("CARGO_PKG_VERSION")`.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools: BTreeMap::new(),
            hooks: BTreeMap::new(),
            on_initialize: None,
            on_shutdown: None,
        }
    }

    /// Register an async tool handler.
    pub fn with_tool<H>(mut self, name: impl Into<String>, handler: H) -> Self
    where
        H: ToolHandler + 'static,
    {
        self.tools.insert(name.into(), Arc::new(handler));
        self
    }

    /// Register an async hook handler. `name` is the hook
    /// (e.g. `"before_message"`); the daemon delivers it as
    /// `hooks/<name>`.
    pub fn with_hook<H>(mut self, name: impl Into<String>, handler: H) -> Self
    where
        H: HookHandler + 'static,
    {
        self.hooks.insert(name.into(), Arc::new(handler));
        self
    }

    /// Optional callback invoked once after the `initialize`
    /// reply is sent. Common use: warm a DB pool, validate env.
    pub fn on_initialize<F, Fut>(mut self, callback: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = SdkResult<()>> + Send + 'static,
    {
        self.on_initialize = Some(Box::pin(move || Box::pin(callback()) as _));
        self
    }

    /// Optional callback invoked when the daemon issues `shutdown`.
    pub fn on_shutdown<F, Fut>(mut self, callback: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_shutdown = Some(Box::pin(move || Box::pin(callback()) as _));
        self
    }

    /// Run the JSON-RPC stdio loop until EOF or `shutdown`.
    pub async fn run_stdio(self) -> SdkResult<()> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        self.run_with(BufReader::new(stdin), stdout).await
    }

    /// Run the JSON-RPC loop on caller-supplied I/O. Useful for
    /// tests, alternative transports, or in-process pipes.
    pub async fn run_with<R, W>(self, reader: R, writer: W) -> SdkResult<()>
    where
        R: AsyncBufRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let handlers = Handlers {
            name: self.name,
            version: self.version,
            tools: self.tools,
            hooks: self.hooks,
        };
        // `on_initialize` / `on_shutdown` callbacks are reserved
        // for a future micro-extension (post-83.4); v0 ignores
        // them so the field exists for the migration story but
        // doesn't change loop semantics.
        let _ = self.on_initialize;
        let _ = self.on_shutdown;
        let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));
        dispatch_loop(reader, writer, handlers).await
    }

    /// Internal: consume the builder into the handler registry.
    /// Public-in-crate so the test harness (feature `test-harness`)
    /// can reach in.
    #[doc(hidden)]
    pub fn into_handlers(self) -> Handlers {
        Handlers {
            name: self.name,
            version: self.version,
            tools: self.tools,
            hooks: self.hooks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::ToolCtx;
    use crate::errors::ToolError;
    use crate::reply::ToolReply;
    use serde_json::Value;

    async fn echo(args: Value, _ctx: ToolCtx) -> std::result::Result<ToolReply, ToolError> {
        Ok(ToolReply::ok_json(args))
    }

    #[test]
    fn with_tool_registers() {
        let app = Microapp::new("t", "0.0.0").with_tool("echo", echo);
        let h = app.into_handlers();
        assert!(h.tools.contains_key("echo"));
    }

    #[test]
    fn chained_with_tool_preserves_each() {
        let app = Microapp::new("t", "0.0.0")
            .with_tool("echo", echo)
            .with_tool("ping", echo);
        let h = app.into_handlers();
        assert!(h.tools.contains_key("echo"));
        assert!(h.tools.contains_key("ping"));
        assert_eq!(h.tools.len(), 2);
    }

    #[test]
    fn name_and_version_passthrough() {
        let app = Microapp::new("agent-creator", "0.1.0");
        let h = app.into_handlers();
        assert_eq!(h.name, "agent-creator");
        assert_eq!(h.version, "0.1.0");
    }
}
