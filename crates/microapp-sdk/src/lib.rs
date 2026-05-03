//! Reusable runtime helpers for Phase 11 stdio microapps consuming
//! the nexo-rs daemon.
//!
//! Phase 83.4. Replaces ~200 LOC of boilerplate every microapp would
//! otherwise rewrite (JSON-RPC line loop, dispatch table, frame
//! encoding, `_meta.nexo.binding` parsing, hook outcome serialisation,
//! tracing setup).
//!
//! # Quick start
//!
//! ```no_run
//! use nexo_microapp_sdk::{Microapp, ToolCtx, ToolReply, ToolError};
//!
//! async fn echo(args: serde_json::Value, _ctx: ToolCtx)
//!     -> Result<ToolReply, ToolError>
//! {
//!     Ok(ToolReply::ok_json(serde_json::json!({"echoed": args})))
//! }
//!
//! #[tokio::main]
//! async fn main() -> nexo_microapp_sdk::Result<()> {
//!     Microapp::new("echo-microapp", env!("CARGO_PKG_VERSION"))
//!         .with_tool("echo", echo)
//!         .run_stdio()
//!         .await
//! }
//! ```

#![deny(missing_docs)]

pub mod builder;
pub mod ctx;
pub mod errors;
pub mod hook;
pub mod logging;
pub mod reply;
pub mod runtime;

#[cfg(feature = "admin")]
pub mod admin;

#[cfg(feature = "notifications")]
pub mod notifications;

#[cfg(feature = "outbound")]
pub mod outbound;

#[cfg(feature = "test-harness")]
pub mod test_harness;

pub use builder::{HandlerRegistry, Microapp};
pub use ctx::{HookCtx, ToolCtx};
pub use errors::{Error, Result, ToolError};
pub use hook::{HookHandler, HookOutcome};
pub use logging::init_logging_from_env;
pub use reply::ToolReply;
pub use runtime::ToolHandler;

#[cfg(feature = "outbound")]
pub use outbound::{DispatchAck, DispatchError, OutboundDispatcher};

#[cfg(feature = "admin")]
pub use admin::{AdminClient, AdminError, AdminSender, DEFAULT_ADMIN_TIMEOUT};

#[cfg(feature = "test-harness")]
pub use test_harness::{MicroappTestError, MicroappTestHarness, MockBindingContext};
