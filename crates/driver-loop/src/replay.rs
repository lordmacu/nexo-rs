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
