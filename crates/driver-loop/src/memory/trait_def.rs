use async_trait::async_trait;
use nexo_driver_claude::ClaudeError;
use nexo_driver_permission::PermissionRequest;
use nexo_driver_types::{Decision, GoalId};

#[async_trait]
pub trait DecisionMemory: Send + Sync + 'static {
    /// Up to `k` past decisions semantically similar to `req`.
    async fn recall(&self, req: &PermissionRequest, k: usize) -> Vec<Decision>;

    /// Persist a decision so it can be recalled later. Best-effort —
    /// implementors that fail to record should log and return Ok so
    /// the decider's hot path doesn't surface storage errors.
    async fn record(&self, _decision: &Decision) -> Result<(), ClaudeError> {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Namespace {
    /// Restrict recall to a specific goal.
    PerGoal(GoalId),
    /// Recall against every decision in the store.
    Global,
}
