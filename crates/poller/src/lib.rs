//! Generic poller subsystem — `crates/poller/`.
//!
//! One runner orchestrates N modules; each module implements
//! [`Poller`] and only contains its fetch / parse / extract logic.
//! Scheduling, jitter, lease-based concurrency control, exponential
//! backoff, per-job circuit breaker, SQLite-backed cursor persistence,
//! telemetry, admin HTTP endpoints, and CLI surface all live in the
//! runner.
//!
//! Post Phase 96 the runner is Laravel-style — the trait knows
//! nothing about channels, credentials, or outbound topics. Pollers
//! reach the world through [`PollerHost`], which is the single point
//! of egress.
//!
//! See `docs/src/recipes/build-a-poller.md` for the three-step DX
//! to plug in a new module.

pub mod admin;
pub mod builtins;
pub mod cli;
pub mod deliver;
pub mod error;
pub mod host;
pub mod in_process_host;
pub mod poller;
pub mod runner;
pub mod schedule;
pub mod state;
pub mod telemetry;

pub use runner::PollerRunner;

pub use deliver::{render_template, DeliverCfg};
pub use error::PollerError;
pub use host::{
    HostError, HostErrorKind, LlmInvokeRequest, LlmInvokeResponse, LlmMessage, LlmUsage, LogLevel,
    PollerHost, TickAck, TickMetrics,
};
pub use in_process_host::InProcessHost;
pub use poller::{CustomToolHandler, CustomToolSpec, PollContext, Poller};
pub use schedule::Schedule;
pub use state::{JobStateSnapshot, PollState};
