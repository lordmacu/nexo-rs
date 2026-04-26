//! B5 + B6 + B7 — `EventForwarder` is a `DriverEventSink` impl that
//! the orchestrator publishes to. On every event it:
//!
//! - B6: feeds `AttemptResult` into `AgentRegistry::apply_attempt`
//!   so `agent_status` / `list_agents` show live turn / acceptance.
//! - B7: pushes a one-line summary into `LogBuffer` so
//!   `agent_logs_tail` returns recent activity.
//! - B5: on `GoalCompleted`, looks up hooks attached to the goal
//!   in `HookRegistry`, computes the right `HookTransition`, and
//!   asks `HookDispatcher::dispatch` to fire each. Failures log
//!   and continue (failure isolation).
//!
//! Wraps an inner `DriverEventSink` so the existing NATS publish
//! pipeline keeps running — production wiring chains:
//!     NatsEventSink ← EventForwarder ← orchestrator
//!
//! This crate already depends on driver-loop; the wrapper lives
//! here (not in driver-loop) because driver-loop must not depend
//! on agent-registry / hook plumbing.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use nexo_agent_registry::{AgentRegistry, LogBuffer};
use nexo_driver_loop::{DriverError, DriverEvent, DriverEventSink};
use nexo_driver_types::AttemptOutcome;

use crate::hooks::dispatcher::HookDispatcher;
use crate::hooks::registry::HookRegistry;
use crate::hooks::types::{HookPayload, HookTransition};

pub struct EventForwarder {
    pub registry: Arc<AgentRegistry>,
    pub log_buffer: Arc<LogBuffer>,
    pub hook_registry: Arc<HookRegistry>,
    pub hook_dispatcher: Arc<dyn HookDispatcher>,
    pub inner: Arc<dyn DriverEventSink>,
}

impl EventForwarder {
    pub fn new(
        registry: Arc<AgentRegistry>,
        log_buffer: Arc<LogBuffer>,
        hook_registry: Arc<HookRegistry>,
        hook_dispatcher: Arc<dyn HookDispatcher>,
        inner: Arc<dyn DriverEventSink>,
    ) -> Self {
        Self {
            registry,
            log_buffer,
            hook_registry,
            hook_dispatcher,
            inner,
        }
    }
}

#[async_trait]
impl DriverEventSink for EventForwarder {
    async fn publish(&self, event: DriverEvent) -> Result<(), DriverError> {
        // 1) Forward to inner sink first (NATS / Noop). Failure
        //    here matches the legacy contract — bubble to caller.
        let inner = self.inner.clone();
        let event_for_inner = event.clone();
        let inner_res = inner.publish(event_for_inner).await;

        // 2) Side effects local to this process.
        match &event {
            DriverEvent::AttemptCompleted { result } => {
                // B6 — refresh snapshot.
                if let Err(e) = self.registry.apply_attempt(result).await {
                    tracing::warn!(target: "event_forwarder", "apply_attempt failed: {e}");
                }
                // B7 — log line.
                let outcome_label = outcome_label(&result.outcome);
                self.log_buffer.push(
                    result.goal_id,
                    "agent.driver.attempt.completed",
                    format!("turn {} → {}", result.turn_index, outcome_label),
                );
            }
            DriverEvent::AttemptStarted {
                goal_id,
                turn_index,
                ..
            } => {
                self.log_buffer.push(
                    *goal_id,
                    "agent.driver.attempt.started",
                    format!("turn {turn_index} starting"),
                );
            }
            DriverEvent::Progress {
                goal_id,
                turn_index,
                ..
            } => {
                self.log_buffer.push(
                    *goal_id,
                    "agent.driver.progress",
                    format!("progress @ turn {turn_index}"),
                );
                // Fire Progress hooks.
                self.fire_hooks_for(*goal_id, HookTransition::Progress)
                    .await;
            }
            DriverEvent::Acceptance { goal_id, verdict } => {
                self.log_buffer.push(
                    *goal_id,
                    "agent.driver.acceptance",
                    format!("acceptance: {}", if verdict.met { "met" } else { "failed" }),
                );
            }
            DriverEvent::GoalCompleted { outcome } => {
                self.log_buffer.push(
                    outcome.goal_id,
                    "agent.driver.goal.completed",
                    format!("goal terminal: {:?}", outcome.outcome),
                );
                // B20 — audit findings (carried in final_text) are
                // logged unconditionally so console-originated
                // dispatches don't lose them when notify_origin
                // no-ops on the 'console' plugin.
                if let Some(text) = &outcome.final_text {
                    let phase = self
                        .registry
                        .handle(outcome.goal_id)
                        .map(|h| h.phase_id)
                        .unwrap_or_default();
                    if phase.starts_with("audit:") {
                        let preview: String = text.chars().take(2000).collect();
                        self.log_buffer.push(
                            outcome.goal_id,
                            "agent.driver.audit.report",
                            preview.clone(),
                        );
                        tracing::info!(
                            target: "audit",
                            goal_id = %outcome.goal_id.0,
                            phase = %phase,
                            "audit report:\n{preview}"
                        );
                    }
                }
                // B5 — fire completion hooks.
                let transition = match &outcome.outcome {
                    AttemptOutcome::Done => HookTransition::Done,
                    AttemptOutcome::Cancelled => HookTransition::Cancelled,
                    _ => HookTransition::Failed,
                };
                self.fire_hooks_for(outcome.goal_id, transition).await;
                // Drop hook entries so the registry doesn't leak.
                self.hook_registry.drop_goal(outcome.goal_id);
            }
            DriverEvent::Escalate { goal_id, reason } => {
                self.log_buffer.push(
                    *goal_id,
                    "agent.driver.escalate",
                    format!("escalate: {reason}"),
                );
            }
            DriverEvent::BudgetExhausted { goal_id, axis, .. } => {
                self.log_buffer.push(
                    *goal_id,
                    "agent.driver.budget.exhausted",
                    format!("budget exhausted: {axis:?}"),
                );
            }
            _ => {}
        }

        inner_res
    }
}

impl EventForwarder {
    async fn fire_hooks_for(&self, goal_id: nexo_driver_types::GoalId, transition: HookTransition) {
        let hooks = self.hook_registry.list(goal_id);
        if hooks.is_empty() {
            return;
        }
        // Build a payload from the live registry handle so the
        // hook gets origin + phase id without us threading them.
        let handle = self.registry.handle(goal_id);
        let phase_id = handle
            .as_ref()
            .map(|h| h.phase_id.clone())
            .unwrap_or_default();
        let origin = handle.as_ref().and_then(|h| h.origin.clone());
        let elapsed = handle
            .as_ref()
            .map(|h| humantime::format_duration(h.elapsed()).to_string())
            .unwrap_or_else(|| "0s".into());
        let summary = handle
            .as_ref()
            .and_then(|h| h.snapshot.last_progress_text.clone())
            .unwrap_or_default();
        let diff_stat = handle
            .as_ref()
            .and_then(|h| h.snapshot.last_diff_stat.clone());
        let payload = HookPayload {
            goal_id,
            phase_id,
            transition,
            summary,
            elapsed,
            diff_stat,
            origin,
        };
        for hook in hooks {
            if !hook.on.matches(transition) {
                continue;
            }
            if let Err(e) = self.hook_dispatcher.dispatch(&hook, &payload).await {
                tracing::warn!(
                    target: "event_forwarder",
                    "hook {} ({}) failed: {e}",
                    hook.id,
                    transition.as_str()
                );
            }
        }
    }
}

fn outcome_label(o: &AttemptOutcome) -> &'static str {
    match o {
        AttemptOutcome::Done => "done",
        AttemptOutcome::NeedsRetry { .. } => "needs_retry",
        AttemptOutcome::Continue { .. } => "continue",
        AttemptOutcome::BudgetExhausted { .. } => "budget_exhausted",
        AttemptOutcome::Cancelled => "cancelled",
        AttemptOutcome::Escalate { .. } => "escalate",
    }
}

// suppress unused chrono import on platforms without the feature
const _: fn() = || {
    let _: chrono::DateTime<Utc> = Utc::now();
};

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_agent_registry::{
        AgentHandle, AgentRunStatus, AgentSnapshot, MemoryAgentRegistryStore,
    };
    use nexo_driver_loop::NoopEventSink;
    use nexo_driver_types::{AttemptResult, BudgetUsage, GoalId};
    use std::sync::Mutex;
    use uuid::Uuid;

    use crate::hooks::types::{CompletionHook, HookAction, HookTrigger};

    #[derive(Default)]
    struct CountingDispatcher {
        fired: Mutex<Vec<(String, HookTransition)>>,
    }
    #[async_trait]
    impl HookDispatcher for CountingDispatcher {
        async fn dispatch(
            &self,
            hook: &CompletionHook,
            payload: &HookPayload,
        ) -> Result<(), crate::hooks::HookError> {
            self.fired
                .lock()
                .unwrap()
                .push((hook.id.clone(), payload.transition));
            Ok(())
        }
    }

    fn handle(id: GoalId) -> AgentHandle {
        AgentHandle {
            goal_id: id,
            phase_id: "67.10".into(),
            status: AgentRunStatus::Running,
            origin: None,
            dispatcher: None,
            started_at: Utc::now(),
            finished_at: None,
            snapshot: AgentSnapshot::default(),
        }
    }

    #[tokio::test]
    async fn attempt_completed_advances_registry_snapshot() {
        let reg = Arc::new(AgentRegistry::new(
            Arc::new(MemoryAgentRegistryStore::default()),
            4,
        ));
        let id = GoalId(Uuid::new_v4());
        reg.admit(handle(id), true).await.unwrap();
        let buf = Arc::new(LogBuffer::new(16));
        let hooks = Arc::new(HookRegistry::new());
        let disp: Arc<dyn HookDispatcher> = Arc::new(CountingDispatcher::default());
        let inner: Arc<dyn DriverEventSink> = Arc::new(NoopEventSink);
        let fwd = EventForwarder::new(reg.clone(), buf.clone(), hooks, disp, inner);

        let ev = DriverEvent::AttemptCompleted {
            result: AttemptResult {
                goal_id: id,
                turn_index: 7,
                outcome: AttemptOutcome::Done,
                decisions_recorded: vec![],
                usage_after: BudgetUsage {
                    turns: 7,
                    ..Default::default()
                },
                acceptance: None,
                final_text: None,
                harness_extras: Default::default(),
            },
        };
        fwd.publish(ev).await.unwrap();
        assert_eq!(reg.snapshot(id).unwrap().turn_index, 7);
        let tail = buf.tail(id, 10);
        assert!(tail.iter().any(|l| l.summary.contains("turn 7")));
    }

    #[tokio::test]
    async fn goal_completed_fires_hooks_and_drops_them() {
        let reg = Arc::new(AgentRegistry::new(
            Arc::new(MemoryAgentRegistryStore::default()),
            4,
        ));
        let id = GoalId(Uuid::new_v4());
        reg.admit(handle(id), true).await.unwrap();
        let hooks = Arc::new(HookRegistry::new());
        hooks.add(
            id,
            CompletionHook {
                id: "h1".into(),
                on: HookTrigger::Done,
                action: HookAction::NotifyOrigin,
            },
        );
        let counting = Arc::new(CountingDispatcher::default());
        let disp: Arc<dyn HookDispatcher> = counting.clone();
        let inner: Arc<dyn DriverEventSink> = Arc::new(NoopEventSink);
        let fwd = EventForwarder::new(
            reg.clone(),
            Arc::new(LogBuffer::new(16)),
            hooks.clone(),
            disp,
            inner,
        );

        // Fire GoalCompleted via an Outcome::Done synthesised
        // GoalOutcome value.
        let outcome = nexo_driver_loop::GoalOutcome {
            goal_id: id,
            outcome: AttemptOutcome::Done,
            total_turns: 5,
            usage: BudgetUsage::default(),
            final_text: None,
            acceptance: None,
            elapsed: std::time::Duration::from_secs(1),
        };
        fwd.publish(DriverEvent::GoalCompleted { outcome })
            .await
            .unwrap();

        let fired = counting.fired.lock().unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].1, HookTransition::Done);
        // Hook entries dropped after firing.
        assert!(hooks.list(id).is_empty());
    }
}
