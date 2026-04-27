//! Phase 71.3 — graceful shutdown drain helper.
//!
//! Walks every `Running` row in the registry, fires
//! `notify_origin` / `notify_channel` hooks with a clean
//! "[shutdown]" `HookPayload`, and flips the row to
//! `LostOnRestart` so a future boot-time sweep does not re-fire
//! the same notification. Each hook dispatch is bounded by a
//! per-hook timeout so a stuck publish cannot hold the daemon's
//! shutdown hostage.
//!
//! Lifted out of `src/main.rs` so the behaviour is unit-testable
//! without spinning a full daemon. The bin's shutdown path is the
//! only production caller, but keeping the function here means
//! Phase 71.4's regression tests can drive it directly with a
//! counting dispatcher.

use std::sync::Arc;
use std::time::Duration;

use nexo_agent_registry::{AgentRegistry, AgentRunStatus};

use crate::hooks::registry::HookRegistry;
use crate::hooks::types::{HookPayload, HookTransition};
use crate::HookDispatcher;

/// Per-hook dispatch budget during shutdown. Long enough for a
/// healthy NATS publish or a single-message Telegram POST,
/// short enough that 50 stale hooks can't add 5 minutes to a
/// SIGTERM. Tunable from the caller if a deployment needs more.
const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(2);

/// Aggregate of what the drain did. `running_seen` is the count of
/// goals that were `Running` when the sweep started; `hooks_fired`
/// is how many hook dispatches actually completed (success or
/// error).
#[derive(Debug, Default, Clone)]
pub struct DrainReport {
    pub running_seen: usize,
    pub hooks_fired: usize,
    pub hook_dispatch_errors: usize,
    pub hook_dispatch_timeouts: usize,
    pub set_status_errors: usize,
}

/// Walk the registry, notify, mark lost. Caller passes the same
/// `Arc`s used by the live runtime so the drain operates on the
/// real tables. `per_hook_timeout = None` falls back to
/// `DEFAULT_HOOK_TIMEOUT`.
pub async fn drain_running_goals(
    registry: &AgentRegistry,
    hooks: &HookRegistry,
    dispatcher: Arc<dyn HookDispatcher>,
    per_hook_timeout: Option<Duration>,
) -> DrainReport {
    let timeout = per_hook_timeout.unwrap_or(DEFAULT_HOOK_TIMEOUT);
    let mut report = DrainReport::default();
    let summaries = match registry.list().await {
        Ok(s) => s,
        Err(_) => return report,
    };
    for summary in summaries {
        if !matches!(summary.status, AgentRunStatus::Running) {
            continue;
        }
        report.running_seen += 1;
        let goal_id = summary.goal_id;
        let Some(handle) = registry.handle(goal_id) else {
            continue;
        };
        let hooks_for_goal = hooks.list(goal_id);
        let payload = HookPayload {
            goal_id,
            phase_id: handle.phase_id.clone(),
            transition: HookTransition::Cancelled,
            summary: format!(
                "[shutdown] daemon stopping — goal `{:?}` was running and has been marked abandoned. \
                 Re-dispatch with `program_phase phase_id={}` if you still need it.",
                goal_id, handle.phase_id,
            ),
            elapsed: humantime::format_duration(handle.elapsed()).to_string(),
            diff_stat: handle.snapshot.last_diff_stat.clone(),
            origin: handle.origin.clone(),
        };
        for hook in hooks_for_goal {
            if !hook.on.matches(HookTransition::Cancelled) {
                continue;
            }
            match tokio::time::timeout(timeout, dispatcher.dispatch(&hook, &payload)).await {
                Ok(Ok(())) => {
                    report.hooks_fired += 1;
                }
                Ok(Err(_)) => {
                    report.hooks_fired += 1;
                    report.hook_dispatch_errors += 1;
                }
                Err(_) => {
                    report.hook_dispatch_timeouts += 1;
                }
            }
        }
        if registry
            .set_status(goal_id, AgentRunStatus::LostOnRestart)
            .await
            .is_err()
        {
            report.set_status_errors += 1;
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nexo_agent_registry::{AgentHandle, AgentSnapshot, MemoryAgentRegistryStore};
    use nexo_driver_types::GoalId;
    use std::sync::Mutex;
    use uuid::Uuid;

    use crate::hooks::types::{CompletionHook, HookAction, HookTrigger};

    #[derive(Default)]
    struct CountingDispatcher {
        fired: Mutex<Vec<(String, String, HookTransition, String)>>,
    }

    #[async_trait::async_trait]
    impl HookDispatcher for CountingDispatcher {
        async fn dispatch(
            &self,
            hook: &CompletionHook,
            payload: &HookPayload,
        ) -> Result<(), crate::HookError> {
            self.fired.lock().unwrap().push((
                hook.id.clone(),
                payload.phase_id.clone(),
                payload.transition,
                payload.summary.clone(),
            ));
            Ok(())
        }
    }

    fn running_handle(phase: &str) -> AgentHandle {
        AgentHandle {
            goal_id: GoalId(Uuid::new_v4()),
            phase_id: phase.into(),
            status: AgentRunStatus::Running,
            origin: None,
            dispatcher: None,
            started_at: Utc::now(),
            finished_at: None,
            snapshot: AgentSnapshot::default(),
        plan_mode: None,
        }
    }

    #[tokio::test]
    async fn drain_fires_cancelled_hook_and_marks_lost() {
        let store = Arc::new(MemoryAgentRegistryStore::default());
        let registry = AgentRegistry::new(store, 4);
        let handle = running_handle("99.7");
        let goal_id = handle.goal_id;
        registry.admit(handle, true).await.unwrap();

        let hooks = HookRegistry::new();
        hooks.add(
            goal_id,
            CompletionHook {
                id: "notify-origin".into(),
                on: HookTrigger::Cancelled,
                action: HookAction::NotifyOrigin,
            },
        );

        let dispatcher = Arc::new(CountingDispatcher::default());
        let report = drain_running_goals(
            &registry,
            &hooks,
            dispatcher.clone(),
            Some(Duration::from_millis(500)),
        )
        .await;

        assert_eq!(report.running_seen, 1);
        assert_eq!(report.hooks_fired, 1);
        assert_eq!(report.hook_dispatch_errors, 0);
        let fired = dispatcher.fired.lock().unwrap().clone();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].0, "notify-origin");
        assert_eq!(fired[0].1, "99.7");
        assert_eq!(fired[0].2, HookTransition::Cancelled);
        assert!(
            fired[0].3.starts_with("[shutdown]"),
            "summary should be tagged for the operator: {}",
            fired[0].3
        );
        let after = registry.list().await.unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].status, AgentRunStatus::LostOnRestart);
    }

    #[tokio::test]
    async fn drain_skips_non_running_goals() {
        let store = Arc::new(MemoryAgentRegistryStore::default());
        let registry = AgentRegistry::new(store, 4);
        let handle = running_handle("99.8");
        let goal_id = handle.goal_id;
        registry.admit(handle, true).await.unwrap();
        registry
            .set_status(goal_id, AgentRunStatus::Done)
            .await
            .unwrap();

        let hooks = HookRegistry::new();
        let dispatcher = Arc::new(CountingDispatcher::default());
        let report = drain_running_goals(&registry, &hooks, dispatcher.clone(), None).await;
        assert_eq!(report.running_seen, 0);
        assert!(dispatcher.fired.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn drain_with_no_matching_hook_still_marks_lost() {
        let store = Arc::new(MemoryAgentRegistryStore::default());
        let registry = AgentRegistry::new(store, 4);
        let handle = running_handle("99.9");
        let goal_id = handle.goal_id;
        registry.admit(handle, true).await.unwrap();
        let hooks = HookRegistry::new();
        // Hook targeting Done (not Cancelled) — should NOT fire,
        // but the row must still flip to LostOnRestart so the boot
        // sweep doesn't re-process it forever.
        hooks.add(
            goal_id,
            CompletionHook {
                id: "wrong-trigger".into(),
                on: HookTrigger::Done,
                action: HookAction::NotifyOrigin,
            },
        );
        let dispatcher = Arc::new(CountingDispatcher::default());
        let report = drain_running_goals(&registry, &hooks, dispatcher.clone(), None).await;
        assert_eq!(report.running_seen, 1);
        assert_eq!(report.hooks_fired, 0);
        assert!(dispatcher.fired.lock().unwrap().is_empty());
        let after = registry.list().await.unwrap();
        assert_eq!(after[0].status, AgentRunStatus::LostOnRestart);
    }
}
