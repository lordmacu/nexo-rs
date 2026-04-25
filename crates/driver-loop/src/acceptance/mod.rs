//! Phase 67.5 — `AcceptanceEvaluator` and friends.

pub mod custom;
pub mod default_evaluator;
pub mod file_match;
pub mod noop;
pub mod shell;
pub mod trait_def;

pub use custom::{CustomVerifier, CustomVerifierRegistry, GitClean, NoPathsTouched};
pub use default_evaluator::DefaultAcceptanceEvaluator;
pub use noop::NoopAcceptanceEvaluator;
pub use shell::{ShellResult, ShellRunner};
pub use trait_def::AcceptanceEvaluator;
