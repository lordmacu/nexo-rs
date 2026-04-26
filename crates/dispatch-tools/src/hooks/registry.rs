//! Phase 67.G.3 — in-memory map keyed by `GoalId` of every hook
//! attached to that goal. Persistence + restart safety lives in the
//! idempotency store (67.F.3); this map is the live "who's
//! listening on this goal" surface that `add_hook` / `remove_hook`
//! / `agent_hooks_list` consume.
//!
//! Kept inside the hooks module rather than in `agent-registry` so
//! the registry crate stays focused on goal lifecycle and doesn't
//! grow a hook-shaped surface that's only consumed here.

use std::sync::Arc;

use dashmap::DashMap;
use nexo_driver_types::GoalId;
use parking_lot::Mutex;

use super::types::CompletionHook;

#[derive(Clone, Default)]
pub struct HookRegistry {
    inner: Arc<DashMap<GoalId, Mutex<Vec<CompletionHook>>>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a hook. Returns the position the new hook was placed at.
    pub fn add(&self, goal_id: GoalId, hook: CompletionHook) -> usize {
        let entry = self.inner.entry(goal_id).or_default();
        let mut v = entry.value().lock();
        v.push(hook);
        v.len() - 1
    }

    /// Remove the hook with the given `id`. Returns `true` when a
    /// hook was actually removed.
    pub fn remove(&self, goal_id: GoalId, hook_id: &str) -> bool {
        let Some(entry) = self.inner.get(&goal_id) else {
            return false;
        };
        let mut v = entry.value().lock();
        let before = v.len();
        v.retain(|h| h.id != hook_id);
        before != v.len()
    }

    /// Snapshot of every hook attached to `goal_id` in declaration
    /// order.
    pub fn list(&self, goal_id: GoalId) -> Vec<CompletionHook> {
        self.inner
            .get(&goal_id)
            .map(|e| e.value().lock().clone())
            .unwrap_or_default()
    }

    /// Drop every hook tied to a goal. Called when the goal reaches
    /// a terminal state and the orchestrator evicts.
    pub fn drop_goal(&self, goal_id: GoalId) {
        self.inner.remove(&goal_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::types::{HookAction, HookTrigger};
    use uuid::Uuid;

    fn hk(id: &str) -> CompletionHook {
        CompletionHook {
            id: id.into(),
            on: HookTrigger::Done,
            action: HookAction::NotifyOrigin,
        }
    }

    #[test]
    fn add_list_remove_round_trip() {
        let reg = HookRegistry::new();
        let g = GoalId(Uuid::new_v4());
        reg.add(g, hk("a"));
        reg.add(g, hk("b"));
        assert_eq!(reg.list(g).len(), 2);
        assert!(reg.remove(g, "a"));
        let l = reg.list(g);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].id, "b");
        assert!(!reg.remove(g, "missing"));
    }

    #[test]
    fn drop_goal_clears_entries() {
        let reg = HookRegistry::new();
        let g = GoalId(Uuid::new_v4());
        reg.add(g, hk("a"));
        reg.drop_goal(g);
        assert!(reg.list(g).is_empty());
    }
}
