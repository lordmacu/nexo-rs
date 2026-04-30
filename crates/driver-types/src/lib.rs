//! Phase 67.0 — types and trait surface for the Nexo driver subsystem.
//!
//! See `crates/driver-types/README.md` for the layering rationale; this
//! file just re-exports the public surface so callers can write
//! `use nexo_driver_types::*;`.

pub mod acceptance;
pub mod attempt;
pub mod auto_dream;
pub mod cancel;
pub mod compact_policy;
pub mod consolidation_lock_probe;
pub mod decision;
pub mod error;
pub mod goal;
pub mod harness;
pub mod memory_checkpoint;
pub mod memory_extractor;
pub mod support;

pub use acceptance::{AcceptanceCriterion, AcceptanceFailure, AcceptanceVerdict};
pub use attempt::{
    AttemptOutcome, AttemptParams, AttemptResult, CompactParams, CompactResult, ResetParams,
    ResetReason,
};
pub use auto_dream::{AutoDreamHook, AutoDreamOutcomeKind, DreamContext as DreamContextLite};
pub use cancel::CancellationToken;
pub use compact_policy::{
    AutoCompactBreaker, CompactContext, CompactPolicy, CompactSummary, CompactSummaryStore,
    CompactTrigger, DefaultCompactPolicy, ExtractMemoriesConfig, SmCompactConfig,
};
pub use decision::{Decision, DecisionChoice, DecisionId};
pub use error::HarnessError;
pub use goal::{BudgetAxis, BudgetGuards, BudgetUsage, Goal, GoalId};
pub use consolidation_lock_probe::ConsolidationLockProbe;
pub use harness::AgentHarness;
pub use memory_checkpoint::MemoryCheckpointer;
pub use memory_extractor::MemoryExtractor;
pub use support::{HarnessRuntime, Support, SupportContext};
