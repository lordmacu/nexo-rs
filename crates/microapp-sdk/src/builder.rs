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
/// Phase 83.4.b.handle — fires once with the live `AdminClient`
/// right before the dispatch loop enters its read loop. Microapp
/// authors capture the handle to reach `nexo/admin/*` from
/// parallel tasks (HTTP server, background workers, etc.) that
/// run alongside the stdio loop.
#[cfg(feature = "admin")]
type AdminReadyCallback =
    Box<dyn FnOnce(Arc<crate::admin::AdminClient>) + Send>;

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
    /// Phase 83.8.8.a — when set, the runtime constructs an
    /// [`crate::admin::AdminClient`] sharing the same line writer
    /// as the daemon-reply path and exposes it through
    /// [`crate::ctx::ToolCtx::admin`] / [`crate::ctx::HookCtx::admin`].
    /// `None` keeps the SDK admin client out of the dispatch loop
    /// (microapps that do not need admin RPC pay zero).
    #[cfg(feature = "admin")]
    pub(crate) admin_enabled: bool,
    /// Phase 83.4.b.handle — optional callback fired once with
    /// the live `AdminClient` right before dispatch_loop starts.
    /// Lets microapps share the client with parallel tasks (HTTP
    /// server, background workers) that run alongside the stdio
    /// loop. `None` skips the callback (default — most microapps
    /// only consume the client through `ctx.admin()`).
    #[cfg(feature = "admin")]
    on_admin_ready: Option<AdminReadyCallback>,
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
            #[cfg(feature = "admin")]
            admin_enabled: false,
            #[cfg(feature = "admin")]
            on_admin_ready: None,
        }
    }

    /// Phase 83.8.8.a — opt the microapp into the admin RPC
    /// surface. The runtime constructs an
    /// [`crate::admin::AdminClient`] sharing the dispatch loop's
    /// line writer, exposes it through
    /// [`crate::ctx::ToolCtx::admin`] / [`crate::ctx::HookCtx::admin`],
    /// and routes inbound frames whose `id` starts with `app:`
    /// to [`crate::admin::AdminClient::on_inbound_response`]
    /// instead of the regular dispatch table.
    ///
    /// Available with the `admin` cargo feature on.
    #[cfg(feature = "admin")]
    pub fn with_admin(mut self) -> Self {
        self.admin_enabled = true;
        self
    }

    /// Phase 83.4.b.handle — register a callback that fires once
    /// with the live `AdminClient` right before the dispatch loop
    /// enters its read loop. Microapp authors capture the handle
    /// to reach `nexo/admin/*` from parallel tasks (HTTP server,
    /// background workers, etc.) that run alongside the stdio
    /// loop.
    ///
    /// The callback is fired only when [`Self::with_admin`] was
    /// also called; otherwise no client exists. Microapps that do
    /// not need a parallel admin handle ignore this method —
    /// `ctx.admin()` inside tool / hook handlers is unaffected.
    ///
    /// Common pattern with a `oneshot` channel so the parallel
    /// task awaits the client's construction:
    ///
    /// ```ignore
    /// let (tx, rx) = tokio::sync::oneshot::channel();
    /// let app = Microapp::new("…", "…")
    ///     .with_admin()
    ///     .on_admin_ready(move |client| { let _ = tx.send(client); });
    /// let stdio = tokio::spawn(app.run_stdio());
    /// let admin = rx.await?;     // ready by the time the loop is up
    /// ```
    #[cfg(feature = "admin")]
    pub fn on_admin_ready<F>(mut self, callback: F) -> Self
    where
        F: FnOnce(Arc<crate::admin::AdminClient>) + Send + 'static,
    {
        self.on_admin_ready = Some(Box::new(callback));
        self
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
            #[cfg(feature = "admin")]
            admin: None,
        };
        // `on_initialize` / `on_shutdown` callbacks are reserved
        // for a future micro-extension (post-83.4); v0 ignores
        // them so the field exists for the migration story but
        // doesn't change loop semantics.
        let _ = self.on_initialize;
        let _ = self.on_shutdown;
        let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));
        // Phase 83.8.8.a — when admin is enabled, build the client
        // that shares this writer so daemon replies + microapp
        // requests do not interleave.
        #[cfg(feature = "admin")]
        let handlers = {
            let mut h = handlers;
            if self.admin_enabled {
                let sender = std::sync::Arc::new(
                    crate::admin::WriterAdminSender::new(writer.clone()),
                );
                let client =
                    std::sync::Arc::new(crate::admin::AdminClient::new(sender));
                // Phase 83.4.b.handle — hand a clone to any
                // registered listener BEFORE entering the
                // dispatch loop so parallel tasks (HTTP servers,
                // background workers) start with the live client
                // already in hand.
                if let Some(cb) = self.on_admin_ready {
                    cb(std::sync::Arc::clone(&client));
                }
                h.admin = Some(client);
            }
            h
        };
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
            #[cfg(feature = "admin")]
            admin: None,
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

    #[cfg(feature = "admin")]
    #[tokio::test]
    async fn on_admin_ready_fires_with_live_client_before_dispatch_loop() {
        // Run a tiny microapp: feed it just an `initialize` request
        // followed by EOF. The callback must fire before the loop
        // returns.
        let (tx, rx) = tokio::sync::oneshot::channel::<Arc<crate::admin::AdminClient>>();
        let app = Microapp::new("t", "0.0.0")
            .with_admin()
            .on_admin_ready(move |client| {
                let _ = tx.send(client);
            });
        // Empty input → loop reads EOF immediately + exits clean.
        let reader = tokio::io::BufReader::new(tokio::io::empty());
        let writer = Vec::new();
        let _ = app.run_with(reader, writer).await;
        // Callback fired — we have a live client.
        assert!(rx.await.is_ok(), "on_admin_ready must fire before run_with returns");
    }

    #[cfg(feature = "admin")]
    #[tokio::test]
    async fn on_admin_ready_does_not_fire_without_with_admin() {
        let (tx, rx) = tokio::sync::oneshot::channel::<Arc<crate::admin::AdminClient>>();
        let app = Microapp::new("t", "0.0.0").on_admin_ready(move |client| {
            let _ = tx.send(client);
        });
        let reader = tokio::io::BufReader::new(tokio::io::empty());
        let writer = Vec::new();
        let _ = app.run_with(reader, writer).await;
        // Callback was registered but admin is disabled — no fire.
        assert!(rx.await.is_err(), "callback must not fire without with_admin()");
    }
}
