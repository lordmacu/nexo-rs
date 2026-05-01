//! Phase 84.3 — worker registry trait + in-memory implementation.
//!
//! `SendMessageToWorker` (Phase 84.3) needs a way to look up a
//! finished worker by its task id and check whether it's eligible
//! for continuation. The registry is the source of truth for that
//! lookup.
//!
//! Registry shape:
//!
//! - keyed by `(coordinator_binding_key, worker_id)` so workers
//!   spawned by one coordinator are invisible to a different
//!   coordinator binding (defense-in-depth — no cross-binding
//!   probing as an existence oracle).
//! - records `WorkerSnapshot { status, message_count, ... }` —
//!   enough metadata for the tool to decide success/refuse without
//!   leaking the actual transcript bytes through the registry
//!   surface.
//! - producer side (registering workers when they start, recording
//!   completion) lands with the fork-as-tool spawn pipeline that
//!   Phase 84.x has not yet built; the registry today exists so the
//!   tool's lookup path can be tested end-to-end via mocks.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

/// Lifecycle state of a worker the coordinator spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStatus {
    /// Worker is mid-turn-loop, no final result yet. Continuation
    /// is refused — coordinator must use `SendToPeer` (peer-to-peer
    /// messaging) instead of `SendMessageToWorker` (which is for
    /// finished workers).
    Running,
    /// Worker reached its goal and produced a final answer.
    Completed,
    /// Worker exited via failure / timeout / kill. Continuation is
    /// allowed — the coordinator may want to ask "what happened?"
    /// or attempt a recovery turn — but the producer side records
    /// this distinct from `Completed` so the tool can include the
    /// prior outcome in its response shape.
    Terminated,
}

impl WorkerStatus {
    /// True when the worker is eligible for `SendMessageToWorker`
    /// continuation. Phase 84.3 spec gates continuation on
    /// finished-not-running.
    pub fn is_continuable(&self) -> bool {
        !matches!(self, WorkerStatus::Running)
    }
}

/// Snapshot of a worker's state at lookup time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSnapshot {
    /// Worker's stable id (the `task_id` field of the
    /// `<task-notification>` envelope, Phase 84.2).
    pub worker_id: String,
    pub status: WorkerStatus,
    /// Coordinator binding that spawned this worker. The lookup
    /// helper enforces this match — workers spawned by binding A
    /// are invisible to binding B.
    pub coordinator_binding_key: String,
    /// Count of messages in the worker's session at completion.
    /// Lets the coordinator's response payload show "resuming a
    /// 12-message session" without leaking the messages
    /// themselves across the trust boundary.
    pub messages_count: usize,
}

/// Outcome of a registry lookup.
///
/// Returning a typed enum (rather than `Option<WorkerSnapshot>` +
/// out-of-band errors) lets the tool handler keep the four error
/// scenarios from the spec each as one match arm, no string
/// comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerLookup {
    /// Worker exists in this binding and is finished — continuation
    /// is allowed. Carries the snapshot for the tool's response
    /// payload.
    Continuable(WorkerSnapshot),
    /// Worker exists in this binding but is still mid-loop. Must
    /// refuse with `WorkerStillRunning` — distinct from
    /// `SendToPeer` semantics (peer is at-rest goal, worker is a
    /// finished forked subagent).
    Running(WorkerSnapshot),
    /// No worker with that id exists in this binding. Could mean
    /// (a) typo / hallucination, (b) worker was registered to a
    /// different binding, or (c) worker has aged out. The tool
    /// returns the same `UnknownWorker` error in all three cases
    /// so the registry doesn't leak existence under a different
    /// binding (defense in depth — no existence oracle).
    Unknown,
}

/// Trait the `SendMessageToWorkerTool` consumes. Production wires
/// the in-memory impl below; tests can swap a mock that scripts
/// each scenario.
#[async_trait]
pub trait WorkerRegistry: Send + Sync {
    /// Look up a worker by `(coordinator_binding_key, worker_id)`.
    /// The binding-key gate is enforced inside the registry; the
    /// caller passes its own binding key from `EffectiveBindingPolicy.binding_id()`.
    async fn lookup(
        &self,
        coordinator_binding_key: &str,
        worker_id: &str,
    ) -> WorkerLookup;
}

/// In-memory registry — DashMap-style HashMap behind a Mutex. Cheap
/// for the typical "tens of workers per coordinator" scale. A
/// SQLite-backed implementation can replace this when durability
/// matters (deferred follow-up).
#[derive(Default)]
pub struct InMemoryWorkerRegistry {
    inner: Mutex<HashMap<(String, String), WorkerSnapshot>>,
}

impl InMemoryWorkerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Producer-side: register or update a worker snapshot.
    /// Called by the spawn pipeline when a worker starts (status =
    /// Running) and again at exit (Completed / Terminated).
    pub fn upsert(&self, snapshot: WorkerSnapshot) {
        let key = (
            snapshot.coordinator_binding_key.clone(),
            snapshot.worker_id.clone(),
        );
        self.inner
            .lock()
            .expect("worker registry mutex poisoned")
            .insert(key, snapshot);
    }

    /// Test/admin helper: drop a worker entry.
    pub fn remove(&self, coordinator_binding_key: &str, worker_id: &str) {
        self.inner
            .lock()
            .expect("worker registry mutex poisoned")
            .remove(&(
                coordinator_binding_key.to_string(),
                worker_id.to_string(),
            ));
    }

    /// Test/admin helper: count entries (for assertions).
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("worker registry mutex poisoned")
            .len()
    }
}

#[async_trait]
impl WorkerRegistry for InMemoryWorkerRegistry {
    async fn lookup(
        &self,
        coordinator_binding_key: &str,
        worker_id: &str,
    ) -> WorkerLookup {
        let guard = self
            .inner
            .lock()
            .expect("worker registry mutex poisoned");
        let key = (coordinator_binding_key.to_string(), worker_id.to_string());
        match guard.get(&key) {
            None => WorkerLookup::Unknown,
            Some(snap) => match snap.status {
                WorkerStatus::Running => WorkerLookup::Running(snap.clone()),
                WorkerStatus::Completed | WorkerStatus::Terminated => {
                    WorkerLookup::Continuable(snap.clone())
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(
        binding: &str,
        worker: &str,
        status: WorkerStatus,
        messages_count: usize,
    ) -> WorkerSnapshot {
        WorkerSnapshot {
            worker_id: worker.to_string(),
            status,
            coordinator_binding_key: binding.to_string(),
            messages_count,
        }
    }

    #[tokio::test]
    async fn lookup_unknown_returns_unknown() {
        let r = InMemoryWorkerRegistry::new();
        assert_eq!(r.lookup("ana:default", "missing").await, WorkerLookup::Unknown);
    }

    #[tokio::test]
    async fn lookup_completed_returns_continuable() {
        let r = InMemoryWorkerRegistry::new();
        let s = snap("ana:default", "w-1", WorkerStatus::Completed, 12);
        r.upsert(s.clone());
        assert_eq!(
            r.lookup("ana:default", "w-1").await,
            WorkerLookup::Continuable(s)
        );
    }

    #[tokio::test]
    async fn lookup_terminated_returns_continuable() {
        let r = InMemoryWorkerRegistry::new();
        let s = snap("ana:default", "w-2", WorkerStatus::Terminated, 5);
        r.upsert(s.clone());
        assert_eq!(
            r.lookup("ana:default", "w-2").await,
            WorkerLookup::Continuable(s)
        );
    }

    #[tokio::test]
    async fn lookup_running_returns_running() {
        let r = InMemoryWorkerRegistry::new();
        let s = snap("ana:default", "w-3", WorkerStatus::Running, 2);
        r.upsert(s.clone());
        assert_eq!(
            r.lookup("ana:default", "w-3").await,
            WorkerLookup::Running(s)
        );
    }

    #[tokio::test]
    async fn lookup_cross_binding_returns_unknown() {
        // Defense in depth: a worker registered under binding A is
        // invisible to binding B. The lookup returns Unknown, NOT
        // Continuable, so binding B can't even prove the worker
        // exists.
        let r = InMemoryWorkerRegistry::new();
        r.upsert(snap("ana:default", "secret-w", WorkerStatus::Completed, 7));
        assert_eq!(
            r.lookup("ana:other", "secret-w").await,
            WorkerLookup::Unknown
        );
        // Sanity: the same worker IS visible under its real binding.
        assert!(matches!(
            r.lookup("ana:default", "secret-w").await,
            WorkerLookup::Continuable(_)
        ));
    }

    #[tokio::test]
    async fn upsert_replaces_existing_entry() {
        let r = InMemoryWorkerRegistry::new();
        r.upsert(snap("a", "w", WorkerStatus::Running, 1));
        r.upsert(snap("a", "w", WorkerStatus::Completed, 8));
        let look = r.lookup("a", "w").await;
        match look {
            WorkerLookup::Continuable(s) => {
                assert_eq!(s.status, WorkerStatus::Completed);
                assert_eq!(s.messages_count, 8);
            }
            other => panic!("expected Continuable, got {other:?}"),
        }
        assert_eq!(r.len(), 1, "upsert must not duplicate");
    }

    #[tokio::test]
    async fn remove_deletes_entry() {
        let r = InMemoryWorkerRegistry::new();
        r.upsert(snap("a", "w", WorkerStatus::Completed, 3));
        assert_eq!(r.len(), 1);
        r.remove("a", "w");
        assert_eq!(r.len(), 0);
        assert_eq!(r.lookup("a", "w").await, WorkerLookup::Unknown);
    }

    #[test]
    fn worker_status_continuable_predicate() {
        assert!(!WorkerStatus::Running.is_continuable());
        assert!(WorkerStatus::Completed.is_continuable());
        assert!(WorkerStatus::Terminated.is_continuable());
    }
}
