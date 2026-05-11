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

// Phase 91 — the STT module ships three mutually-compatible
// backends: legacy whisper-rs (`stt`), Candle pure-Rust
// (`stt-candle`), and cloud REST (`stt-cloud`). Each carries
// its own feature flag; combining them at the call site is
// `CompositeProvider`'s job.
//
// Phase 91.x.wasm.phase-3 — the local inference paths (`stt`
// whisper-rs + `stt-candle` Candle ML stack) depend on heavy
// native crates (`opus-wave`, `ogg`, `candle-{core,nn,
// transformers}`, `tokenizers`, plus tokio's `fs` + `io-std`
// surface) that don't compile on `wasm32-unknown-unknown`. The
// per-submodule cfg gates inside `stt/mod.rs` keep them out of
// WASM builds.
//
// Phase 91.x.wasm.phase-4 — the cloud backend (`stt-cloud`)
// uses `reqwest`, which swaps to the browser fetch API on
// wasm32. So `stt-cloud` alone is the path for WASM consumers
// who want real STT; `stt` / `stt-candle` give them the wire
// types but the inference modules drop out.
//
// Parent gate accepts any one of the three features; inner
// modules in `stt/mod.rs` apply the correct per-backend +
// target cfg.
#[cfg(any(feature = "stt", feature = "stt-candle", feature = "stt-cloud-wasm"))]
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

#[cfg(feature = "compose-draft")]
pub mod compose_draft;

#[cfg(feature = "db-migrate")]
pub mod db_migrate;

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
