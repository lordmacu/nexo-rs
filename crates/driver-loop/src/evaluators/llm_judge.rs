//! Phase 87.1 — LLM-as-judge `AcceptanceEvaluator`.
//!
//! Routes `AcceptanceCriterion::LlmJudge` variants to a forked
//! subagent running the judge persona prompt. The judge reads
//! `(criterion_text, worker_output, transcript_summary)` and
//! returns JSON `{verdict: "pass"|"fail", reasons: [string]}`.
//!
//! The actual fork dispatch + LlmClient wiring lives behind a
//! [`JudgeBackend`] trait that 87.1.b implements with the real
//! `nexo-fork` machinery. This crate-internal module ships the
//! parser, the persona prompt asset, the trait, and the
//! `AcceptanceEvaluator` impl that converts a verdict into the
//! Phase 67/68 [`AcceptanceFailure`] / pass shape.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_driver_types::{AcceptanceCriterion, AcceptanceFailure, AcceptanceVerdict};
use serde::{Deserialize, Serialize};

use crate::acceptance::AcceptanceEvaluator;
use crate::error::DriverError;

/// The judge's structured response. Wire form is JSON; the
/// orchestrator's parser tolerates surrounding chatter (the model
/// occasionally wraps the JSON in ```json fences) and falls back
/// to `Fail { reasons: ["malformed judge response"] }` on parse
/// errors so a misbehaving judge never silently passes a goal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "lowercase")]
pub enum JudgeVerdict {
    Pass,
    Fail,
}

/// Full parsed response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeResponse {
    #[serde(flatten)]
    pub verdict: JudgeVerdict,
    #[serde(default)]
    pub reasons: Vec<String>,
}

/// Errors the judge backend can surface to the evaluator. The
/// evaluator's policy is "any error → fail with reason logged" so
/// a judge timeout never silently passes a goal.
#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    #[error("judge LLM call failed: {0}")]
    LlmFailure(String),
    #[error("judge timed out")]
    Timeout,
    #[error("judge produced malformed output: {0}")]
    Malformed(String),
}

/// Provider-agnostic backend the evaluator delegates to. Real
/// implementation (LLM dispatch via `nexo-fork`) lands in 87.1.b;
/// tests pass a closure-backed mock.
#[async_trait]
pub trait JudgeBackend: Send + Sync + 'static {
    /// Render the judge prompt + dispatch the call. Returns the
    /// raw text the judge produced (the evaluator parses it via
    /// [`parse_judge_response`]).
    async fn ask(
        &self,
        criterion_text: &str,
        worker_output: &str,
        transcript_summary: &str,
    ) -> Result<String, JudgeError>;
}

/// Phase 87.1 — `AcceptanceEvaluator` that routes `LlmJudge`
/// criteria to its [`JudgeBackend`]. Other criterion variants are
/// passed through to a fallback evaluator (typically the default
/// rules-based one) so an operator can mix-and-match.
pub struct LlmJudgeEvaluator {
    pub backend: Arc<dyn JudgeBackend>,
    /// Fallback for non-LlmJudge criteria. Built so the evaluator
    /// composes cleanly: ShellCommand still goes through
    /// `DefaultAcceptanceEvaluator`; LlmJudge gets the model.
    pub fallback: Arc<dyn AcceptanceEvaluator>,
    /// Snapshot of the worker's final output the judge inspects.
    /// Phase 87.1 ships this as a per-instance field; 87.1.b will
    /// thread it from the orchestrator's per-goal context.
    pub worker_output: String,
    /// Optional transcript summary the judge sees alongside the
    /// criterion. Empty = "no transcript".
    pub transcript_summary: String,
}

impl LlmJudgeEvaluator {
    pub fn new(
        backend: Arc<dyn JudgeBackend>,
        fallback: Arc<dyn AcceptanceEvaluator>,
        worker_output: impl Into<String>,
        transcript_summary: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            fallback,
            worker_output: worker_output.into(),
            transcript_summary: transcript_summary.into(),
        }
    }

    /// Run the judge against one criterion. Returns
    /// `Some(failure)` when the judge says fail (or errors); `None`
    /// when the judge says pass.
    async fn evaluate_one(
        &self,
        idx: usize,
        criterion_text: &str,
    ) -> Result<Option<AcceptanceFailure>, DriverError> {
        let raw = match self
            .backend
            .ask(
                criterion_text,
                &self.worker_output,
                &self.transcript_summary,
            )
            .await
        {
            Ok(s) => s,
            Err(e) => {
                // Any backend error → fail-closed. The operator
                // sees the error message in the verdict's
                // `failures` list, so the judge's silence does
                // not silently pass a goal.
                return Ok(Some(AcceptanceFailure {
                    criterion_index: idx,
                    criterion_label: "llm_judge".to_string(),
                    message: format!("judge unavailable: {e}"),
                    evidence: None,
                }));
            }
        };

        match parse_judge_response(&raw) {
            Some(JudgeResponse {
                verdict: JudgeVerdict::Pass,
                ..
            }) => Ok(None),
            Some(JudgeResponse {
                verdict: JudgeVerdict::Fail,
                reasons,
            }) => Ok(Some(AcceptanceFailure {
                criterion_index: idx,
                criterion_label: "llm_judge".to_string(),
                message: if reasons.is_empty() {
                    "judge returned `fail` without reasons".to_string()
                } else {
                    reasons.join("; ")
                },
                evidence: Some(truncate(&raw, 512)),
            })),
            None => Ok(Some(AcceptanceFailure {
                criterion_index: idx,
                criterion_label: "llm_judge".to_string(),
                message: "judge produced malformed JSON — treating as fail".to_string(),
                evidence: Some(truncate(&raw, 512)),
            })),
        }
    }
}

#[async_trait]
impl AcceptanceEvaluator for LlmJudgeEvaluator {
    async fn evaluate(
        &self,
        criteria: &[AcceptanceCriterion],
        workspace: &Path,
    ) -> Result<AcceptanceVerdict, DriverError> {
        let started = std::time::Instant::now();

        // Split: LlmJudge criteria run via the backend; everything
        // else delegates to the fallback evaluator. Two passes so
        // we preserve the criterion-index alignment of the
        // failure list.
        let mut failures: Vec<AcceptanceFailure> = Vec::new();
        let mut non_judge: Vec<AcceptanceCriterion> = Vec::with_capacity(criteria.len());
        let mut non_judge_idx_map: Vec<usize> = Vec::with_capacity(criteria.len());

        for (idx, c) in criteria.iter().enumerate() {
            match c {
                AcceptanceCriterion::LlmJudge { criterion_text } => {
                    if let Some(f) = self.evaluate_one(idx, criterion_text).await? {
                        failures.push(f);
                    }
                }
                _ => {
                    non_judge.push(c.clone());
                    non_judge_idx_map.push(idx);
                }
            }
        }

        if !non_judge.is_empty() {
            let v = self.fallback.evaluate(&non_judge, workspace).await?;
            for mut f in v.failures {
                let original = non_judge_idx_map
                    .get(f.criterion_index)
                    .copied()
                    .unwrap_or(f.criterion_index);
                f.criterion_index = original;
                failures.push(f);
            }
        }

        Ok(AcceptanceVerdict {
            met: failures.is_empty(),
            failures,
            evaluated_at: chrono::Utc::now(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// Best-effort JSON parser for the judge's response. Tolerates
/// surrounding text (the model occasionally wraps the JSON in
/// `\`\`\`json` fences or adds preamble); returns `None` when no
/// valid `JudgeResponse` shape can be extracted, so the caller
/// fails the criterion rather than silently passing it.
pub fn parse_judge_response(raw: &str) -> Option<JudgeResponse> {
    if let Ok(parsed) = serde_json::from_str::<JudgeResponse>(raw.trim()) {
        return Some(parsed);
    }
    // Try to recover an embedded JSON object — find the first `{`
    // and the matching `}` (naive: stop at first balanced close).
    let start = raw.find('{')?;
    let mut depth = 0_i32;
    let mut end = None;
    for (i, ch) in raw[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let chunk = &raw[start..end?];
    serde_json::from_str(chunk).ok()
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acceptance::NoopAcceptanceEvaluator;
    use std::sync::Mutex;

    /// Mock backend scripted by a `Vec` of responses (one per
    /// `ask` call). Each call pops the next entry from the front.
    struct ScriptedBackend {
        responses: Mutex<Vec<Result<String, JudgeError>>>,
    }

    impl ScriptedBackend {
        fn new(responses: Vec<Result<String, JudgeError>>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl JudgeBackend for ScriptedBackend {
        async fn ask(
            &self,
            _criterion_text: &str,
            _worker_output: &str,
            _transcript_summary: &str,
        ) -> Result<String, JudgeError> {
            let mut g = self.responses.lock().unwrap();
            g.remove(0)
        }
    }

    fn evaluator(scripted: Vec<Result<String, JudgeError>>) -> LlmJudgeEvaluator {
        LlmJudgeEvaluator::new(
            Arc::new(ScriptedBackend::new(scripted)),
            Arc::new(NoopAcceptanceEvaluator),
            "worker did the thing",
            "summary: 3 turns, 2 file edits",
        )
    }

    #[test]
    fn parse_pass_verdict() {
        let raw = r#"{"verdict":"pass","reasons":[]}"#;
        let r = parse_judge_response(raw).expect("pass parse");
        assert_eq!(r.verdict, JudgeVerdict::Pass);
        assert!(r.reasons.is_empty());
    }

    #[test]
    fn parse_fail_verdict_with_reasons() {
        let raw = r#"{"verdict":"fail","reasons":["missing null check","no test added"]}"#;
        let r = parse_judge_response(raw).expect("fail parse");
        assert_eq!(r.verdict, JudgeVerdict::Fail);
        assert_eq!(r.reasons.len(), 2);
    }

    #[test]
    fn parse_recovers_from_fenced_json() {
        // Models commonly wrap structured output in code fences.
        let raw = "```json\n{\"verdict\":\"pass\",\"reasons\":[]}\n```";
        let r = parse_judge_response(raw).expect("fenced parse");
        assert_eq!(r.verdict, JudgeVerdict::Pass);
    }

    #[test]
    fn parse_returns_none_for_garbage() {
        assert!(parse_judge_response("yes it passes").is_none());
        assert!(parse_judge_response("").is_none());
        assert!(parse_judge_response("{not_json").is_none());
    }

    #[test]
    fn parse_missing_reasons_defaults_to_empty() {
        let raw = r#"{"verdict":"pass"}"#;
        let r = parse_judge_response(raw).expect("parse");
        assert!(r.reasons.is_empty());
    }

    #[tokio::test]
    async fn evaluator_pass_yields_met_verdict() {
        let e = evaluator(vec![Ok(
            r#"{"verdict":"pass","reasons":[]}"#.to_string()
        )]);
        let crit = vec![AcceptanceCriterion::llm_judge("must add null check")];
        let v = e
            .evaluate(&crit, std::path::Path::new("."))
            .await
            .expect("evaluate");
        assert!(v.met);
        assert!(v.failures.is_empty());
    }

    #[tokio::test]
    async fn evaluator_fail_yields_failure_with_reasons() {
        let e = evaluator(vec![Ok(
            r#"{"verdict":"fail","reasons":["null check absent","no regression test"]}"#
                .to_string(),
        )]);
        let crit = vec![AcceptanceCriterion::llm_judge("must add null check")];
        let v = e
            .evaluate(&crit, std::path::Path::new("."))
            .await
            .expect("evaluate");
        assert!(!v.met);
        assert_eq!(v.failures.len(), 1);
        assert_eq!(v.failures[0].criterion_label, "llm_judge");
        assert!(v.failures[0].message.contains("null check absent"));
        assert!(v.failures[0].message.contains("no regression test"));
    }

    #[tokio::test]
    async fn evaluator_malformed_response_treated_as_fail() {
        let e = evaluator(vec![Ok("yeah looks good to me".to_string())]);
        let crit = vec![AcceptanceCriterion::llm_judge("must add null check")];
        let v = e
            .evaluate(&crit, std::path::Path::new("."))
            .await
            .expect("evaluate");
        assert!(!v.met);
        assert_eq!(v.failures.len(), 1);
        assert!(v.failures[0].message.contains("malformed"));
    }

    #[tokio::test]
    async fn evaluator_timeout_treated_as_fail() {
        let e = evaluator(vec![Err(JudgeError::Timeout)]);
        let crit = vec![AcceptanceCriterion::llm_judge("rule")];
        let v = e
            .evaluate(&crit, std::path::Path::new("."))
            .await
            .expect("evaluate");
        assert!(!v.met);
        assert!(v.failures[0].message.contains("timed out"));
    }

    #[tokio::test]
    async fn evaluator_routes_only_llm_judge_to_backend() {
        // Mix of LlmJudge and ShellCommand. The backend is
        // scripted with ONE response (the LlmJudge); the
        // ShellCommand goes through the fallback (Noop, which
        // always passes). If the mix routes wrong, the scripted
        // backend exhausts its queue and panics on the second
        // `remove(0)`.
        let e = evaluator(vec![Ok(
            r#"{"verdict":"pass","reasons":[]}"#.to_string()
        )]);
        let crit = vec![
            AcceptanceCriterion::llm_judge("must add null check"),
            AcceptanceCriterion::shell("cargo test"),
        ];
        let v = e
            .evaluate(&crit, std::path::Path::new("."))
            .await
            .expect("evaluate");
        assert!(v.met);
    }

    #[tokio::test]
    async fn evaluator_fail_without_reasons_surfaces_default_message() {
        let e = evaluator(vec![Ok(r#"{"verdict":"fail"}"#.to_string())]);
        let crit = vec![AcceptanceCriterion::llm_judge("rule")];
        let v = e
            .evaluate(&crit, std::path::Path::new("."))
            .await
            .expect("evaluate");
        assert!(!v.met);
        assert!(v.failures[0].message.contains("without reasons"));
    }
}
