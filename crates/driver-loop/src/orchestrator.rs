//! Goal-level loop. Takes a `Goal`, drives it to completion through
//! the per-turn attempt loop. Emits NATS events at every major
//! transition.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nexo_driver_claude::SessionBindingStore;
use nexo_driver_permission::PermissionDecider;
use nexo_driver_types::{
    AcceptanceVerdict, AttemptOutcome, AttemptParams, BudgetGuards, BudgetUsage, CancellationToken,
    Goal, GoalId,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken as TokioCancel;

use crate::acceptance::{AcceptanceEvaluator, NoopAcceptanceEvaluator};
use crate::attempt::{run_attempt, AttemptContext};
use crate::error::DriverError;
use crate::events::{DriverEvent, DriverEventSink, NoopEventSink};
use crate::mcp_config::write_mcp_config;
use crate::socket::DriverSocketServer;
use crate::workspace::WorkspaceManager;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalOutcome {
    pub goal_id: GoalId,
    pub outcome: AttemptOutcome,
    pub total_turns: u32,
    pub usage: BudgetUsage,
    pub final_text: Option<String>,
    pub acceptance: Option<AcceptanceVerdict>,
    #[serde(with = "humantime_serde")]
    pub elapsed: Duration,
}

pub struct DriverOrchestrator {
    claude_cfg: nexo_driver_claude::ClaudeConfig,
    binding_store: Arc<dyn SessionBindingStore>,
    acceptance: Arc<dyn AcceptanceEvaluator>,
    workspace_manager: Arc<WorkspaceManager>,
    event_sink: Arc<dyn DriverEventSink>,
    bin_path: PathBuf,
    socket_path: PathBuf,
    /// Owns the spawned socket server; cancelling kills it.
    _socket_handle: tokio::task::JoinHandle<Result<(), DriverError>>,
    socket_cancel: TokioCancel,
    cancel_root: CancellationToken,
}

#[derive(Default)]
pub struct DriverOrchestratorBuilder {
    claude_cfg: Option<nexo_driver_claude::ClaudeConfig>,
    binding_store: Option<Arc<dyn SessionBindingStore>>,
    acceptance: Option<Arc<dyn AcceptanceEvaluator>>,
    decider: Option<Arc<dyn PermissionDecider>>,
    workspace_manager: Option<Arc<WorkspaceManager>>,
    event_sink: Option<Arc<dyn DriverEventSink>>,
    bin_path: Option<PathBuf>,
    socket_path: Option<PathBuf>,
    cancel_root: Option<CancellationToken>,
}

impl DriverOrchestratorBuilder {
    pub fn claude_config(mut self, cfg: nexo_driver_claude::ClaudeConfig) -> Self {
        self.claude_cfg = Some(cfg);
        self
    }
    pub fn binding_store(mut self, s: Arc<dyn SessionBindingStore>) -> Self {
        self.binding_store = Some(s);
        self
    }
    pub fn acceptance(mut self, a: Arc<dyn AcceptanceEvaluator>) -> Self {
        self.acceptance = Some(a);
        self
    }
    pub fn decider(mut self, d: Arc<dyn PermissionDecider>) -> Self {
        self.decider = Some(d);
        self
    }
    pub fn workspace_manager(mut self, w: Arc<WorkspaceManager>) -> Self {
        self.workspace_manager = Some(w);
        self
    }
    pub fn event_sink(mut self, e: Arc<dyn DriverEventSink>) -> Self {
        self.event_sink = Some(e);
        self
    }
    pub fn bin_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.bin_path = Some(p.into());
        self
    }
    pub fn socket_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.socket_path = Some(p.into());
        self
    }
    pub fn cancel_root(mut self, c: CancellationToken) -> Self {
        self.cancel_root = Some(c);
        self
    }

    pub async fn build(self) -> Result<DriverOrchestrator, DriverError> {
        let claude_cfg = self
            .claude_cfg
            .ok_or_else(|| DriverError::Config("claude config required".into()))?;
        let binding_store = self
            .binding_store
            .ok_or_else(|| DriverError::Config("binding_store required".into()))?;
        let decider = self
            .decider
            .ok_or_else(|| DriverError::Config("decider required".into()))?;
        let workspace_manager = self
            .workspace_manager
            .ok_or_else(|| DriverError::Config("workspace_manager required".into()))?;
        let bin_path = self
            .bin_path
            .ok_or_else(|| DriverError::Config("bin_path required".into()))?;
        let socket_path = self
            .socket_path
            .ok_or_else(|| DriverError::Config("socket_path required".into()))?;
        let acceptance: Arc<dyn AcceptanceEvaluator> = self
            .acceptance
            .unwrap_or_else(|| Arc::new(NoopAcceptanceEvaluator));
        let event_sink: Arc<dyn DriverEventSink> =
            self.event_sink.unwrap_or_else(|| Arc::new(NoopEventSink));
        let cancel_root = self.cancel_root.unwrap_or_default();

        // Bind the socket server.
        let socket_cancel = TokioCancel::new();
        let server = DriverSocketServer::bind(&socket_path, decider, socket_cancel.clone()).await?;
        let socket_handle = tokio::spawn(server.run());

        Ok(DriverOrchestrator {
            claude_cfg,
            binding_store,
            acceptance,
            workspace_manager,
            event_sink,
            bin_path,
            socket_path,
            _socket_handle: socket_handle,
            socket_cancel,
            cancel_root,
        })
    }
}

impl DriverOrchestrator {
    pub fn builder() -> DriverOrchestratorBuilder {
        DriverOrchestratorBuilder::default()
    }

    /// Drive a single goal to completion. Long-running.
    pub async fn run_goal(&self, goal: Goal) -> Result<GoalOutcome, DriverError> {
        let started = Instant::now();
        let goal_id = goal.id;

        let _ = self
            .event_sink
            .publish(DriverEvent::GoalStarted { goal: goal.clone() })
            .await;

        // 1. Workspace + mcp config.
        let workspace = self.workspace_manager.ensure(&goal).await?;
        let mcp_config_path = write_mcp_config(&workspace, &self.bin_path, &self.socket_path)?;

        // 2. Loop turns.
        let mut usage = BudgetUsage::default();
        let mut prior_failures: Vec<nexo_driver_types::AcceptanceFailure> = Vec::new();
        let mut last_acceptance: Option<AcceptanceVerdict> = None;
        let mut final_text: Option<String> = None;
        let mut total_turns: u32 = 0;
        let final_outcome: AttemptOutcome;

        loop {
            if let Some(axis) = goal.budget.is_exhausted(&usage) {
                let _ = self
                    .event_sink
                    .publish(DriverEvent::BudgetExhausted {
                        goal_id,
                        axis,
                        usage: usage.clone(),
                    })
                    .await;
                final_outcome = AttemptOutcome::BudgetExhausted { axis };
                break;
            }
            if self.cancel_root.is_cancelled() {
                final_outcome = AttemptOutcome::Cancelled;
                break;
            }

            let _ = self
                .event_sink
                .publish(DriverEvent::AttemptStarted {
                    goal_id,
                    turn_index: total_turns,
                    usage: usage.clone(),
                })
                .await;

            let cancel = self.cancel_root.clone();
            let params = AttemptParams {
                goal: goal.clone(),
                turn_index: total_turns,
                usage: usage.clone(),
                prior_decisions: Vec::new(),
                cancel,
                extras: build_attempt_extras(&prior_failures, &goal.budget, total_turns),
            };

            let ctx = AttemptContext {
                claude_cfg: &self.claude_cfg,
                binding_store: &self.binding_store,
                acceptance: &self.acceptance,
                workspace: &workspace,
                mcp_config_path: &mcp_config_path,
                bin_path: &self.bin_path,
                cancel: self.cancel_root.clone(),
            };
            let result = run_attempt(ctx, params).await?;
            usage = result.usage_after.clone();
            final_text = result.final_text.clone();
            last_acceptance = result.acceptance.clone();
            total_turns += 1;
            usage.turns = total_turns;

            let _ = self
                .event_sink
                .publish(DriverEvent::AttemptCompleted {
                    result: result.clone(),
                })
                .await;
            if let Some(v) = &result.acceptance {
                let _ = self
                    .event_sink
                    .publish(DriverEvent::Acceptance {
                        goal_id,
                        verdict: v.clone(),
                    })
                    .await;
            }

            match &result.outcome {
                AttemptOutcome::Done => {
                    final_outcome = AttemptOutcome::Done;
                    break;
                }
                AttemptOutcome::NeedsRetry { failures } => {
                    prior_failures = failures.clone();
                    continue;
                }
                AttemptOutcome::Continue { .. } => {
                    // session-invalid retry / mid-conversation pause /
                    // stream-ended-without-result: retry next turn.
                    prior_failures.clear();
                    continue;
                }
                AttemptOutcome::Cancelled => {
                    final_outcome = AttemptOutcome::Cancelled;
                    break;
                }
                AttemptOutcome::BudgetExhausted { axis } => {
                    let _ = self
                        .event_sink
                        .publish(DriverEvent::BudgetExhausted {
                            goal_id,
                            axis: *axis,
                            usage: usage.clone(),
                        })
                        .await;
                    final_outcome = AttemptOutcome::BudgetExhausted { axis: *axis };
                    break;
                }
                AttemptOutcome::Escalate { reason } => {
                    let _ = self
                        .event_sink
                        .publish(DriverEvent::Escalate {
                            goal_id,
                            reason: reason.clone(),
                        })
                        .await;
                    final_outcome = AttemptOutcome::Escalate {
                        reason: reason.clone(),
                    };
                    break;
                }
            }
        }

        let outcome = GoalOutcome {
            goal_id,
            outcome: final_outcome,
            total_turns,
            usage,
            final_text,
            acceptance: last_acceptance,
            elapsed: started.elapsed(),
        };
        let _ = self
            .event_sink
            .publish(DriverEvent::GoalCompleted {
                outcome: outcome.clone(),
            })
            .await;
        Ok(outcome)
    }

    /// Cancel every in-flight goal + drain socket server.
    pub async fn shutdown(self) -> Result<(), DriverError> {
        self.cancel_root.cancel();
        self.socket_cancel.cancel();
        let _ = self._socket_handle.await;
        Ok(())
    }
}

fn build_attempt_extras(
    prior_failures: &[nexo_driver_types::AcceptanceFailure],
    budget: &BudgetGuards,
    turn_index: u32,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert(
        "turn_index".into(),
        serde_json::Value::Number(turn_index.into()),
    );
    m.insert(
        "max_turns".into(),
        serde_json::Value::Number(budget.max_turns.into()),
    );
    if !prior_failures.is_empty() {
        m.insert(
            "prior_failures".into(),
            serde_json::to_value(prior_failures).unwrap_or(serde_json::Value::Null),
        );
    }
    m
}
