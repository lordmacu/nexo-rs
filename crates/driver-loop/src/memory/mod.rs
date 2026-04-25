//! Phase 67.4 + 67.7 — `DecisionMemory` trait and impls.

/// Deterministic embedder for tests. Hidden from public docs but
/// reachable from integration tests in `tests/`.
#[doc(hidden)]
pub mod mock;

pub mod noop;
pub mod prompt;
pub mod sqlite_vec;
pub mod trait_def;

pub use noop::NoopDecisionMemory;
pub use sqlite_vec::SqliteVecDecisionMemory;
pub use trait_def::{DecisionMemory, Namespace};
