//! Phase 87.1 — LLM-as-judge AcceptanceEvaluator implementations.
//!
//! Sister to `crate::acceptance` (the default rules-based
//! evaluator that handles `ShellCommand` / `FileMatches` /
//! `Custom`). This module hosts evaluators that delegate the
//! verdict to an LLM rather than to a typed rule.

pub mod llm_judge;

pub use llm_judge::{
    parse_judge_response, LlmJudgeEvaluator, JudgeBackend, JudgeError, JudgeResponse,
    JudgeVerdict,
};
