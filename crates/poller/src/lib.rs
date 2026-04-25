//! Generic poller subsystem — `crates/poller/`.
//!
//! One runner orchestrates N modules; each module implements
//! [`Poller`] and only contains its fetch / parse / extract logic.
//! Scheduling, jitter, lease-based concurrency control, exponential
//! backoff, per-job circuit breaker, SQLite-backed cursor persistence,
//! credential resolution (Phase 17), outbound dispatch, telemetry,
//! admin HTTP endpoints, and CLI surface all live in the runner.
//!
//! See `docs/src/recipes/build-a-poller.md` for the three-step DX
//! to plug in a new module.

pub mod builtins;
pub mod dispatch;
pub mod error;
pub mod poller;
pub mod runner;
pub mod schedule;
pub mod state;
pub mod telemetry;

pub use runner::PollerRunner;

pub use error::PollerError;
pub use poller::{OutboundDelivery, PollContext, Poller, TickOutcome};
pub use schedule::Schedule;
pub use state::{JobStateSnapshot, PollState};
