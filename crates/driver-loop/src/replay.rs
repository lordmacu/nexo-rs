//! Phase 67.8 — replay-policy. Classifies mid-turn errors observed by
//! `attempt::run_attempt` into a `ReplayDecision` the orchestrator
//! acts on (mark_invalid, rollback, bump consecutive_errors).

use async_trait::async_trait;
use nexo_driver_types::{BudgetUsage, GoalId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayOutcomeHint {
    /// `attempt::run_attempt` returned `Continue { reason }`.
    Continue,
    /// `attempt::run_attempt` returned `Escalate { reason }`.
    Escalate,
}

#[derive(Clone, Debug)]
pub struct ReplayContext<'a> {
    pub goal_id: GoalId,
    pub turn_index: u32,
    pub pre_turn_checkpoint: Option<&'a str>,
    pub usage: &'a BudgetUsage,
    pub error_message: &'a str,
    pub last_outcome_hint: ReplayOutcomeHint,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplayDecision {
    /// Mark binding invalid + (optional) rollback + retry the same
    /// turn without bumping `turn_index`. Bumps `consecutive_errors`.
    FreshSessionRetry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rollback_to: Option<String>,
    },
    /// Continue to the next turn; resets `consecutive_errors` when
    /// the orchestrator sees a successful subsequent outcome.
    NextTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rollback_to: Option<String>,
    },
    /// Phase 85.1 — provider returned `PromptTooLong` (413). Force a
    /// compact pass (`Trigger::Reactive413`) without consulting the
    /// proactive estimator, then retry the same turn once. Bumps
    /// `consecutive_413`; reset by any successful turn. Distinct
    /// from `FreshSessionRetry` so the orchestrator routes to
    /// the compact subsystem instead of a session re-bind.
    CompactAndRetry,
    /// Hard stop with `Escalate { reason }`.
    Escalate { reason: String },
}

#[async_trait]
pub trait ReplayPolicy: Send + Sync + 'static {
    async fn classify(&self, ctx: &ReplayContext<'_>) -> ReplayDecision;
}

pub struct DefaultReplayPolicy {
    pub max_fresh_session_retries: u32,
}

impl Default for DefaultReplayPolicy {
    fn default() -> Self {
        Self {
            max_fresh_session_retries: 1,
        }
    }
}

#[async_trait]
impl ReplayPolicy for DefaultReplayPolicy {
    async fn classify(&self, ctx: &ReplayContext<'_>) -> ReplayDecision {
        let msg = ctx.error_message.to_lowercase();
        let mentions_session = msg.contains("session");
        let session_dead = mentions_session
            && (msg.contains("not found") || msg.contains("expired") || msg.contains("invalid"));
        let transient = msg.contains("timeout")
            || msg.contains("rate limit")
            || msg.contains("unavailable")
            || msg.contains("temporarily");
        // Phase 85.1 — reactive 413 recovery.
        let prompt_too_long = msg.contains("prompt too long")
            || msg.contains("prompt_too_long")
            || msg.contains("context_length_exceeded")
            || msg.contains("payload_too_large");

        if prompt_too_long {
            // Cap stays in `BudgetGuards.max_consecutive_413`; the
            // orchestrator checks `is_exhausted` and converts a hit
            // to `Escalate { reason: "consecutive_413 exceeded" }`
            // before re-entering the loop. Replay policy itself
            // returns `CompactAndRetry` unconditionally so the
            // budget-axis check is centralized.
            return ReplayDecision::CompactAndRetry;
        }

        if session_dead {
            return ReplayDecision::FreshSessionRetry {
                rollback_to: ctx.pre_turn_checkpoint.map(str::to_owned),
            };
        }
        if transient {
            if ctx.usage.consecutive_errors < self.max_fresh_session_retries {
                return ReplayDecision::FreshSessionRetry {
                    rollback_to: ctx.pre_turn_checkpoint.map(str::to_owned),
                };
            }
            return ReplayDecision::Escalate {
                reason: format!(
                    "transient errors persist after {} retries: {}",
                    self.max_fresh_session_retries, ctx.error_message
                ),
            };
        }
        ReplayDecision::NextTurn { rollback_to: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(msg: &'a str, usage: &'a BudgetUsage, cp: Option<&'a str>) -> ReplayContext<'a> {
        ReplayContext {
            goal_id: GoalId::new(),
            turn_index: 0,
            pre_turn_checkpoint: cp,
            usage,
            error_message: msg,
            last_outcome_hint: ReplayOutcomeHint::Continue,
        }
    }

    #[tokio::test]
    async fn session_not_found_returns_fresh_session_retry() {
        let pol = DefaultReplayPolicy::default();
        let usage = BudgetUsage::default();
        let d = pol
            .classify(&ctx(
                "Session not found, please retry",
                &usage,
                Some("abc1234"),
            ))
            .await;
        match d {
            ReplayDecision::FreshSessionRetry { rollback_to } => {
                assert_eq!(rollback_to.as_deref(), Some("abc1234"));
            }
            other => panic!("expected FreshSessionRetry, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_expired_returns_fresh_session_retry() {
        let pol = DefaultReplayPolicy::default();
        let usage = BudgetUsage::default();
        let d = pol
            .classify(&ctx("session expired, re-auth needed", &usage, None))
            .await;
        assert!(matches!(d, ReplayDecision::FreshSessionRetry { .. }));
    }

    #[tokio::test]
    async fn transient_under_cap_returns_fresh_session_retry() {
        let pol = DefaultReplayPolicy {
            max_fresh_session_retries: 2,
        };
        let usage = BudgetUsage {
            consecutive_errors: 0,
            ..Default::default()
        };
        let d = pol
            .classify(&ctx("Request timeout from upstream", &usage, None))
            .await;
        assert!(matches!(d, ReplayDecision::FreshSessionRetry { .. }));
    }

    #[tokio::test]
    async fn transient_over_cap_returns_escalate() {
        let pol = DefaultReplayPolicy {
            max_fresh_session_retries: 1,
        };
        let usage = BudgetUsage {
            consecutive_errors: 1,
            ..Default::default()
        };
        let d = pol.classify(&ctx("Rate limit hit", &usage, None)).await;
        match d {
            ReplayDecision::Escalate { reason } => {
                assert!(reason.contains("transient"));
            }
            other => panic!("expected Escalate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_error_returns_next_turn() {
        let pol = DefaultReplayPolicy::default();
        let usage = BudgetUsage::default();
        let d = pol
            .classify(&ctx(
                "compilation aborted unexpectedly",
                &usage,
                Some("xyz"),
            ))
            .await;
        match d {
            ReplayDecision::NextTurn { rollback_to } => {
                assert!(rollback_to.is_none(), "default NextTurn must not rollback");
            }
            other => panic!("expected NextTurn, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn prompt_too_long_returns_compact_and_retry() {
        // Phase 85.1 spec test 1: classification of `PromptTooLong`.
        let pol = DefaultReplayPolicy::default();
        let usage = BudgetUsage::default();
        let d = pol
            .classify(&ctx(
                "prompt too long: 220000 / 200000",
                &usage,
                Some("abc"),
            ))
            .await;
        assert_eq!(d, ReplayDecision::CompactAndRetry);
    }

    #[tokio::test]
    async fn prompt_too_long_with_provider_phrasing_classifies() {
        // Provider variants: Anthropic uses `prompt_too_long`,
        // OpenAI-compat uses `context_length_exceeded`,
        // generic 413 says `payload_too_large`. All three route to
        // CompactAndRetry.
        let pol = DefaultReplayPolicy::default();
        let usage = BudgetUsage::default();
        for phrase in [
            "prompt_too_long: input is too long",
            "context_length_exceeded: 220000 tokens",
            "payload_too_large",
        ] {
            let d = pol.classify(&ctx(phrase, &usage, None)).await;
            assert_eq!(
                d,
                ReplayDecision::CompactAndRetry,
                "phrase `{phrase}` did not classify as CompactAndRetry"
            );
        }
    }

    #[tokio::test]
    async fn compact_and_retry_classification_does_not_consult_consecutive_413_cap() {
        // Phase 85.1 spec test 2: budget axis exhaustion is
        // centralised in the orchestrator (BudgetGuards.is_exhausted),
        // NOT inside the classifier. The classifier returns
        // CompactAndRetry unconditionally; the orchestrator decides
        // when to convert to Escalate via the budget axis.
        let pol = DefaultReplayPolicy::default();
        let usage = BudgetUsage {
            consecutive_413: 99,
            ..Default::default()
        };
        let d = pol.classify(&ctx("prompt too long", &usage, None)).await;
        assert_eq!(d, ReplayDecision::CompactAndRetry);
    }

    #[tokio::test]
    async fn prompt_too_long_short_circuits_other_classifiers() {
        // Phase 85.1 spec test 3: when an error mentions both
        // "session" markers and "prompt too long", the 413
        // classifier wins (no double-route into FreshSessionRetry).
        let pol = DefaultReplayPolicy::default();
        let usage = BudgetUsage::default();
        let d = pol
            .classify(&ctx(
                "session expired AND prompt too long",
                &usage,
                Some("xyz"),
            ))
            .await;
        assert_eq!(d, ReplayDecision::CompactAndRetry);
    }

    #[tokio::test]
    async fn no_pre_turn_checkpoint_yields_none_rollback() {
        let pol = DefaultReplayPolicy::default();
        let usage = BudgetUsage::default();
        let d = pol.classify(&ctx("session not found", &usage, None)).await;
        assert!(matches!(
            d,
            ReplayDecision::FreshSessionRetry { rollback_to: None }
        ));
    }
}
