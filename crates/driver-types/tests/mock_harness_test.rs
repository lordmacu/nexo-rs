//! Smoke test that the trait is usable behind `Arc<dyn AgentHarness>`
//! and that an impl can return any of the outcome variants.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexo_driver_types::*;
use uuid::Uuid;

struct EchoHarness;

#[async_trait]
impl AgentHarness for EchoHarness {
    fn id(&self) -> &str {
        "echo"
    }
    fn label(&self) -> &str {
        "Echo (test-only)"
    }
    fn supports(&self, _ctx: &SupportContext) -> Support {
        Support::Supported {
            priority: 1,
            reason: None,
        }
    }
    async fn run_attempt(&self, params: AttemptParams) -> Result<AttemptResult, HarnessError> {
        Ok(AttemptResult {
            goal_id: params.goal.id,
            turn_index: params.turn_index,
            outcome: AttemptOutcome::Done,
            decisions_recorded: vec![],
            usage_after: params.usage,
            acceptance: None,
            final_text: Some("ok".into()),
            harness_extras: serde_json::Map::new(),
        })
    }
}

fn _assert_object_safe<T: ?Sized + Send + Sync>() {}

#[tokio::test]
async fn dyn_harness_can_run_an_attempt() {
    _assert_object_safe::<dyn AgentHarness>();

    let h: Arc<dyn AgentHarness> = Arc::new(EchoHarness);
    assert_eq!(h.id(), "echo");

    let goal = Goal {
        id: GoalId(Uuid::nil()),
        description: "smoke".into(),
        acceptance: vec![],
        budget: BudgetGuards {
            max_turns: 1,
            max_wall_time: Duration::from_secs(60),
            max_tokens: 1000,
            max_consecutive_denies: 1,
            max_consecutive_errors: 5,
            max_consecutive_413: 2,
        },
        workspace: None,
        metadata: serde_json::Map::new(),
    };
    let params = AttemptParams {
        goal,
        turn_index: 0,
        usage: BudgetUsage::default(),
        prior_decisions: vec![],
        cancel: CancellationToken::new(),
        extras: serde_json::Map::new(),
    };
    let res = h.run_attempt(params).await.unwrap();
    assert!(matches!(res.outcome, AttemptOutcome::Done));
}

#[tokio::test]
async fn defaults_for_compact_reset_dispose_succeed() {
    let h: Arc<dyn AgentHarness> = Arc::new(EchoHarness);

    let cr = h
        .compact(CompactParams {
            goal_id: GoalId(Uuid::nil()),
            focus: None,
        })
        .await
        .unwrap();
    assert!(matches!(cr, CompactResult::Skipped { .. }));

    h.reset(ResetParams {
        goal_id: None,
        reason: ResetReason::Idle,
    })
    .await
    .unwrap();

    h.dispose().await.unwrap();
}
