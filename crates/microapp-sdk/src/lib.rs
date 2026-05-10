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
pub mod locale;
pub mod logging;
pub mod reply;
pub mod runtime;

#[cfg(feature = "admin")]
pub mod admin;

#[cfg(feature = "notifications")]
pub mod notifications;

#[cfg(feature = "events")]
pub mod events;

#[cfg(feature = "stt")]
pub mod stt;

#[cfg(feature = "wizard")]
pub mod wizard;

#[cfg(feature = "voice")]
pub mod voice;

#[cfg(feature = "identity")]
pub mod identity;

#[cfg(feature = "routing")]
pub mod routing;

#[cfg(feature = "enrichment")]
pub mod enrichment;

#[cfg(feature = "outbound")]
pub mod outbound;

#[cfg(feature = "tracking")]
pub mod tracking;

#[cfg(feature = "scoring")]
pub mod scoring;

#[cfg(feature = "guardrails")]
pub mod guardrails;

#[cfg(feature = "templating")]
pub mod templating;

#[cfg(feature = "email-threading")]
pub mod email_threading;

#[cfg(feature = "email-template")]
pub mod email_template;

#[cfg(feature = "module-state")]
pub mod module_state;

#[cfg(feature = "compose-attachment")]
pub mod compose_attachment;

#[cfg(feature = "dedup")]
pub mod dedup;

#[cfg(feature = "plugin")]
pub mod plugin;

#[cfg(feature = "test-harness")]
pub mod test_harness;

pub use builder::{HandlerRegistry, Microapp};
pub use ctx::{HookCtx, ToolCtx};
pub use errors::{Error, Result, ToolError};
pub use hook::{HookHandler, HookOutcome};
pub use locale::{LangCode, Locale, LocaleParseError, RegionCode};
pub use logging::init_logging_from_env;
pub use reply::ToolReply;
pub use runtime::ToolHandler;

#[cfg(feature = "outbound")]
pub use outbound::{DispatchAck, DispatchError, OutboundDispatcher};

#[cfg(feature = "plugin")]
pub use plugin::{
    BrokerEventHandler, BrokerSender, LlmCompleteParams, LlmCompleteResult, LlmStream,
    PluginAdapter, RpcError, ShutdownHandler, TokenCount, ToolContext, ToolHandlerWithContext,
};
// Re-export the broker event type so `on_broker_event` callers
// can name the closure parameter without depending on
// `nexo-broker` directly. Lift introduced when the marketing
// extension needed `Event` for its inbound subscriber.
#[cfg(feature = "plugin")]
pub use nexo_broker::Event as BrokerEvent;

#[cfg(feature = "admin")]
pub use admin::{AdminClient, AdminError, AdminSender, DEFAULT_ADMIN_TIMEOUT};

#[cfg(feature = "test-harness")]
pub use test_harness::{MicroappTestError, MicroappTestHarness, MockBindingContext};
