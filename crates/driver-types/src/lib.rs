//! Phase 67.0 — types and trait surface for the Nexo driver subsystem.
//!
//! See `crates/driver-types/README.md` for the layering rationale; this
//! file just re-exports the public surface so callers can write
//! `use nexo_driver_types::*;`.

pub mod acceptance;
pub mod attempt;
pub mod cancel;
pub mod decision;
pub mod error;
pub mod goal;
pub mod harness;
pub mod support;

pub use acceptance::{AcceptanceCriterion, AcceptanceFailure, AcceptanceVerdict};
pub use attempt::{
    AttemptOutcome, AttemptParams, AttemptResult, CompactParams, CompactResult, ResetParams,
    ResetReason,
};
pub use cancel::CancellationToken;
pub use decision::{Decision, DecisionChoice, DecisionId};
pub use error::HarnessError;
pub use goal::{BudgetAxis, BudgetGuards, BudgetUsage, Goal, GoalId};
pub use harness::AgentHarness;
pub use support::{HarnessRuntime, Support, SupportContext};
