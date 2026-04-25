//! `DriverEvent` enum + sink trait. Subjects when wired to NATS:
//! `agent.driver.{goal,attempt}.{started,completed}`,
//! `agent.driver.{decision,acceptance,budget.exhausted,escalate}`.

use async_trait::async_trait;
use nexo_driver_types::{
    AcceptanceVerdict, AttemptResult, BudgetAxis, BudgetUsage, Decision, Goal, GoalId,
};
use serde::{Deserialize, Serialize};

use crate::error::DriverError;
use crate::orchestrator::GoalOutcome;
use crate::replay::ReplayDecision;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DriverEvent {
    GoalStarted {
        goal: Goal,
    },
    GoalCompleted {
        outcome: GoalOutcome,
    },
    AttemptStarted {
        goal_id: GoalId,
        turn_index: u32,
        usage: BudgetUsage,
    },
    AttemptCompleted {
        result: AttemptResult,
    },
    Decision {
        decision: Decision,
    },
    Acceptance {
        goal_id: GoalId,
        verdict: AcceptanceVerdict,
    },
    BudgetExhausted {
        goal_id: GoalId,
        axis: BudgetAxis,
        usage: BudgetUsage,
    },
    Escalate {
        goal_id: GoalId,
        reason: String,
    },
    /// Phase 67.8 — replay-policy classified a mid-turn error.
    ReplayDecision {
        goal_id: GoalId,
        turn_index: u32,
        decision: ReplayDecision,
        error_message: String,
    },
    /// Phase 67.9 — orchestrator scheduled a `/compact` turn.
    CompactRequested {
        goal_id: GoalId,
        turn_index: u32,
        focus: String,
        token_pressure: f64,
    },
    /// Phase 67.C.1 — periodic mid-run progress beacon. Fires every
    /// `progress_every_turns` after an `AttemptCompleted`, so chat
    /// hooks (`on: progress`) and admin-ui can show 'still going'
    /// without waiting for the goal to finish.
    Progress {
        goal_id: GoalId,
        turn_index: u32,
        usage: BudgetUsage,
        last_text: Option<String>,
    },
}

impl DriverEvent {
    /// NATS subject for this event kind.
    pub fn nats_subject(&self) -> &'static str {
        match self {
            DriverEvent::GoalStarted { .. } => "agent.driver.goal.started",
            DriverEvent::GoalCompleted { .. } => "agent.driver.goal.completed",
            DriverEvent::AttemptStarted { .. } => "agent.driver.attempt.started",
            DriverEvent::AttemptCompleted { .. } => "agent.driver.attempt.completed",
            DriverEvent::Decision { .. } => "agent.driver.decision",
            DriverEvent::Acceptance { .. } => "agent.driver.acceptance",
            DriverEvent::BudgetExhausted { .. } => "agent.driver.budget.exhausted",
            DriverEvent::Escalate { .. } => "agent.driver.escalate",
            DriverEvent::ReplayDecision { .. } => "agent.driver.replay",
            DriverEvent::CompactRequested { .. } => "agent.driver.compact",
            DriverEvent::Progress { .. } => "agent.driver.progress",
        }
    }
}

#[async_trait]
pub trait DriverEventSink: Send + Sync + 'static {
    async fn publish(&self, event: DriverEvent) -> Result<(), DriverError>;
}

#[derive(Default)]
pub struct NoopEventSink;

#[async_trait]
impl DriverEventSink for NoopEventSink {
    async fn publish(&self, _event: DriverEvent) -> Result<(), DriverError> {
        Ok(())
    }
}

#[cfg(feature = "nats")]
pub struct NatsEventSink {
    client: async_nats::Client,
}

#[cfg(feature = "nats")]
impl NatsEventSink {
    pub fn new(client: async_nats::Client) -> Self {
        Self { client }
    }
}

#[cfg(feature = "nats")]
#[async_trait]
impl DriverEventSink for NatsEventSink {
    async fn publish(&self, event: DriverEvent) -> Result<(), DriverError> {
        let subject = event.nats_subject().to_string();
        let payload = serde_json::to_vec(&event)?;
        self.client
            .publish(subject, payload.into())
            .await
            .map_err(|e| DriverError::Nats(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_driver_types::AttemptOutcome;
    use uuid::Uuid;

    #[tokio::test]
    async fn noop_sink_always_ok() {
        let s = NoopEventSink;
        s.publish(DriverEvent::Escalate {
            goal_id: GoalId(Uuid::nil()),
            reason: "x".into(),
        })
        .await
        .unwrap();
    }

    #[test]
    fn nats_subjects_stable() {
        let g = GoalId(Uuid::nil());
        let cases: Vec<(DriverEvent, &str)> = vec![
            (
                DriverEvent::Escalate {
                    goal_id: g,
                    reason: "x".into(),
                },
                "agent.driver.escalate",
            ),
            (
                DriverEvent::BudgetExhausted {
                    goal_id: g,
                    axis: BudgetAxis::Turns,
                    usage: BudgetUsage::default(),
                },
                "agent.driver.budget.exhausted",
            ),
            (
                DriverEvent::Progress {
                    goal_id: g,
                    turn_index: 5,
                    usage: BudgetUsage::default(),
                    last_text: None,
                },
                "agent.driver.progress",
            ),
        ];
        for (e, want) in cases {
            assert_eq!(e.nats_subject(), want);
        }
    }

    #[test]
    fn driver_event_round_trips_json() {
        let e = DriverEvent::AttemptCompleted {
            result: AttemptResult {
                goal_id: GoalId(Uuid::nil()),
                turn_index: 0,
                outcome: AttemptOutcome::Done,
                decisions_recorded: vec![],
                usage_after: BudgetUsage::default(),
                acceptance: None,
                final_text: None,
                harness_extras: serde_json::Map::new(),
            },
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: DriverEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.nats_subject(), e.nats_subject());
    }
}
