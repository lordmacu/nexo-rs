//! `AgentRegistry` façade — single in-memory source of truth for
//! every goal that has been admitted into the driver subsystem.
//!
//! Two layers:
//!
//! - In-process map keyed by `GoalId`, holding an `Arc<RegistryEntry>`
//!   whose `snapshot: ArcSwap<AgentSnapshot>` lets readers
//!   (`list_agents`, `agent_status`) see the latest turn / acceptance
//!   without locking writers.
//! - Optional persistent store (`AgentRegistryStore`) — every state
//!   transition is mirrored to disk so reattach (67.B.4) can
//!   rehydrate `Running` goals after a daemon restart.
//!
//! Capacity + queue: `admit()` enforces a global cap. Beyond it, the
//! caller chooses to enqueue (FIFO) or reject. `release()` (called
//! when a goal terminates) pops the next queued goal and returns it
//! so the caller can hand it to the orchestrator.

use std::collections::VecDeque;
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::Utc;
use dashmap::DashMap;
use nexo_driver_types::{AcceptanceVerdict, AttemptResult, GoalId};
use parking_lot::Mutex;

use crate::store::AgentRegistryStore;
use crate::types::{
    AgentHandle, AgentRunStatus, AgentSleepState, AgentSnapshot, AgentSummary, AskPendingState,
    RegistryError,
};

/// Outcome of an `admit()` call.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AdmitOutcome {
    /// Slot was available; goal is now `Running`.
    Admitted,
    /// Cap reached and `enqueue=true`; goal is `Queued` at position N.
    Queued { position: usize },
    /// Cap reached and `enqueue=false`.
    Rejected,
}

/// Internal entry — wraps the persistable handle behind an
/// `ArcSwap<AgentSnapshot>` so readers never block on a writer.
#[derive(Clone)]
struct RegistryEntry {
    handle_meta: Arc<Mutex<HandleMeta>>,
    snapshot: Arc<ArcSwap<AgentSnapshot>>,
}

/// Mutable parts of `AgentHandle` that aren't the live snapshot.
/// `phase_id` / `started_at` are immutable for the goal's lifetime
/// so they live outside the lock for cheap reads.
#[derive(Clone)]
struct HandleMeta {
    phase_id: String,
    status: AgentRunStatus,
    origin: Option<nexo_driver_claude::OriginChannel>,
    dispatcher: Option<nexo_driver_claude::DispatcherIdentity>,
    started_at: chrono::DateTime<Utc>,
    finished_at: Option<chrono::DateTime<Utc>>,
}

pub struct AgentRegistry {
    inner: DashMap<GoalId, RegistryEntry>,
    queue: Mutex<VecDeque<GoalId>>,
    /// Phase 67-PT-5: serialises the read-then-write decision in
    /// `admit` so two concurrent dispatchers cannot each observe
    /// `count_running < cap` and both end up Admitted (overshoot
    /// of N = number of in-flight admits). Reads are still
    /// lock-free; only the admission path takes this lock.
    admit_lock: tokio::sync::Mutex<()>,
    /// Wrapped in `RwLock` so the admin tool `set_concurrency_cap`
    /// can mutate it without rebuilding the registry. Reads are
    /// hot-path (`admit`, `count_running`) so `RwLock` over `Mutex`
    /// keeps cap reads parallel.
    cap: parking_lot::RwLock<u32>,
    store: Arc<dyn AgentRegistryStore>,
}

impl AgentRegistry {
    pub fn new(store: Arc<dyn AgentRegistryStore>, cap: u32) -> Self {
        Self {
            inner: DashMap::new(),
            queue: Mutex::new(VecDeque::new()),
            admit_lock: tokio::sync::Mutex::new(()),
            cap: parking_lot::RwLock::new(cap),
            store,
        }
    }

    pub fn cap(&self) -> u32 {
        *self.cap.read()
    }

    /// Phase 67.G.4 — operator override for the global cap.
    /// Returns the new value. Lowering the cap below the live
    /// running count does not pre-empt; existing goals keep going,
    /// new admissions land in the queue until count drops back
    /// under cap.
    pub fn set_cap(&self, new_cap: u32) -> u32 {
        *self.cap.write() = new_cap;
        new_cap
    }

    /// Drop every queued goal (those still parked, not yet promoted).
    /// Returns the count drained. Running goals untouched.
    pub async fn flush_queue(&self) -> Result<u64, RegistryError> {
        let drained: Vec<GoalId> = {
            let mut q = self.queue.lock();
            q.drain(..).collect()
        };
        let n = drained.len() as u64;
        for g in drained {
            // Mark as Cancelled so list_agents shows the operator's
            // intent rather than the goal sitting silently in
            // memory.
            self.set_status(g, AgentRunStatus::Cancelled).await?;
        }
        Ok(n)
    }

    /// Evict terminal handles older than `cutoff` from the
    /// in-memory map AND from the persistent store.
    pub async fn evict_terminal_older_than(
        &self,
        cutoff: chrono::DateTime<Utc>,
    ) -> Result<u64, RegistryError> {
        let victims: Vec<GoalId> = self
            .inner
            .iter()
            .filter(|e| {
                let m = e.value().handle_meta.lock();
                m.status.is_terminal() && m.finished_at.map(|t| t < cutoff).unwrap_or(false)
            })
            .map(|e| *e.key())
            .collect();
        for g in &victims {
            self.inner.remove(g);
        }
        let n_mem = victims.len() as u64;
        let n_store = self.store.evict_terminal_older_than(cutoff).await?;
        Ok(n_mem.max(n_store))
    }

    /// Try to admit a fresh goal. If the cap is reached, either
    /// enqueue (FIFO) or reject depending on `enqueue`.
    pub async fn admit(
        &self,
        handle: AgentHandle,
        enqueue: bool,
    ) -> Result<AdmitOutcome, RegistryError> {
        // PT-5 + B16 — serialise the cap decision + queue mutation
        // + in-memory entry insert under admit_lock, then drop the
        // lock BEFORE the persistent store upsert. The store call
        // can block on disk IO; holding admit_lock across it would
        // gate every concurrent admit on the slowest one.
        let goal_id = handle.goal_id;
        let h_for_store: AgentHandle;
        let outcome = {
            let _guard = self.admit_lock.lock().await;
            let running = self.count_running();
            let outcome = if running >= *self.cap.read() {
                if !enqueue {
                    return Ok(AdmitOutcome::Rejected);
                }
                let mut q = self.queue.lock();
                q.push_back(goal_id);
                let position = q.len();
                AdmitOutcome::Queued { position }
            } else {
                AdmitOutcome::Admitted
            };

            let mut h = handle;
            h.status = match outcome {
                AdmitOutcome::Admitted => AgentRunStatus::Running,
                AdmitOutcome::Queued { .. } => AgentRunStatus::Queued,
                AdmitOutcome::Rejected => unreachable!(),
            };
            h.snapshot.sleep = None;
            let entry = RegistryEntry {
                handle_meta: Arc::new(Mutex::new(HandleMeta {
                    phase_id: h.phase_id.clone(),
                    status: h.status,
                    origin: h.origin.clone(),
                    dispatcher: h.dispatcher.clone(),
                    started_at: h.started_at,
                    finished_at: h.finished_at,
                })),
                snapshot: Arc::new(ArcSwap::from_pointee(h.snapshot.clone())),
            };
            self.inner.insert(goal_id, entry);
            h_for_store = h;
            outcome
        }; // admit_lock dropped here.
        self.store.upsert(&h_for_store).await?;
        Ok(outcome)
    }

    /// Promote a queued goal to `Running` — called by the orchestrator
    /// once the dispatch infrastructure is ready to spawn it. Returns
    /// `false` if the goal was not queued (already running, or unknown).
    pub async fn promote_queued(&self, goal_id: GoalId) -> Result<bool, RegistryError> {
        {
            let mut q = self.queue.lock();
            if !q.iter().any(|g| *g == goal_id) {
                return Ok(false);
            }
            q.retain(|g| *g != goal_id);
        }
        self.set_status(goal_id, AgentRunStatus::Running).await?;
        Ok(true)
    }

    /// Mark a goal terminal and surface the next queued goal (if any).
    pub async fn release(
        &self,
        goal_id: GoalId,
        terminal: AgentRunStatus,
    ) -> Result<Option<GoalId>, RegistryError> {
        if !terminal.is_terminal() {
            return Err(RegistryError::InvalidTransition {
                from: AgentRunStatus::Running,
                to: terminal,
            });
        }
        // Set finished_at + status, persist.
        if let Some(entry) = self.inner.get(&goal_id) {
            let mut meta = entry.handle_meta.lock();
            meta.status = terminal;
            meta.finished_at = Some(Utc::now());
        } else {
            return Err(RegistryError::NotFound(goal_id));
        }
        self.clear_sleep_snapshot(goal_id);
        self.persist(goal_id).await?;
        // B12 — atomic pop-and-mark-Running of the next queued
        // goal. Two callers racing release would otherwise both
        // see the same head and both try to promote it; this
        // moves the head out of the queue under the queue lock
        // before returning, so each release returns the slot to
        // exactly one caller. Caller is still expected to call
        // promote_queued(id) so the registry's status flips —
        // we leave that step explicit so the caller can short-
        // circuit if the orchestrator setup fails.
        let next = self.queue.lock().pop_front();
        Ok(next)
    }

    /// Apply a status change in-place. The caller is responsible for
    /// validating the transition; we accept anything here so
    /// `pause` / `resume` can move between Running ↔ Paused freely.
    pub async fn set_status(
        &self,
        goal_id: GoalId,
        status: AgentRunStatus,
    ) -> Result<(), RegistryError> {
        let entry = self
            .inner
            .get(&goal_id)
            .ok_or(RegistryError::NotFound(goal_id))?;
        {
            let mut meta = entry.handle_meta.lock();
            meta.status = status;
            if status.is_terminal() && meta.finished_at.is_none() {
                meta.finished_at = Some(Utc::now());
            }
        }
        drop(entry);
        if !matches!(status, AgentRunStatus::Sleeping) {
            self.clear_sleep_snapshot(goal_id);
        }
        self.persist(goal_id).await
    }

    /// Phase 77.20.3 — mark a goal as parked by proactive Sleep and
    /// persist the wake metadata separately from the turn transcript.
    pub async fn set_sleeping(
        &self,
        goal_id: GoalId,
        wake_at: chrono::DateTime<Utc>,
        duration_ms: u64,
        reason: String,
    ) -> Result<(), RegistryError> {
        let entry = self
            .inner
            .get(&goal_id)
            .ok_or(RegistryError::NotFound(goal_id))?;
        {
            let mut meta = entry.handle_meta.lock();
            meta.status = AgentRunStatus::Sleeping;
        }
        let cur = (**entry.snapshot.load()).clone();
        entry.snapshot.store(Arc::new(AgentSnapshot {
            sleep: Some(AgentSleepState {
                wake_at,
                duration_ms,
                reason,
            }),
            last_event_at: Utc::now(),
            ..cur
        }));
        drop(entry);
        self.persist(goal_id).await
    }

    /// Clear any persisted Sleep metadata and mark the goal running.
    /// Called when the orchestrator starts the wake-up/tick turn.
    pub async fn clear_sleeping(&self, goal_id: GoalId) -> Result<(), RegistryError> {
        let entry = self
            .inner
            .get(&goal_id)
            .ok_or(RegistryError::NotFound(goal_id))?;
        {
            let mut meta = entry.handle_meta.lock();
            if matches!(meta.status, AgentRunStatus::Sleeping) {
                meta.status = AgentRunStatus::Running;
            }
        }
        drop(entry);
        self.clear_sleep_snapshot(goal_id);
        self.persist(goal_id).await
    }

    /// Live snapshot. Cheap — readers never wait.
    pub fn snapshot(&self, goal_id: GoalId) -> Option<AgentSnapshot> {
        self.inner
            .get(&goal_id)
            .map(|e| (**e.snapshot.load()).clone())
    }

    /// Compose a full `AgentHandle` for `agent_status`.
    pub fn handle(&self, goal_id: GoalId) -> Option<AgentHandle> {
        let entry = self.inner.get(&goal_id)?;
        let meta = entry.handle_meta.lock().clone();
        let snap = (**entry.snapshot.load()).clone();
        Some(AgentHandle {
            goal_id,
            phase_id: meta.phase_id,
            status: meta.status,
            origin: meta.origin,
            dispatcher: meta.dispatcher,
            started_at: meta.started_at,
            finished_at: meta.finished_at,
            snapshot: snap,
            plan_mode: None,
            kind: crate::SessionKind::Interactive,
        })
    }

    /// Find the newest paused goal matching an origin tuple.
    /// Used by AskUserQuestion reply routing to resume the waiting goal.
    pub fn find_paused_by_origin(
        &self,
        plugin: &str,
        instance: &str,
        sender_id: &str,
    ) -> Option<GoalId> {
        self.inner
            .iter()
            .filter_map(|e| {
                let meta = e.value().handle_meta.lock().clone();
                if meta.status != AgentRunStatus::Paused {
                    return None;
                }
                let origin = meta.origin?;
                if origin.plugin == plugin
                    && origin.instance == instance
                    && origin.sender_id == sender_id
                {
                    Some((*e.key(), meta.started_at))
                } else {
                    None
                }
            })
            .max_by_key(|(_, started)| *started)
            .map(|(goal_id, _)| goal_id)
    }

    /// Find a paused goal by explicit AskUserQuestion id + origin tuple.
    pub fn find_paused_by_question_id(
        &self,
        plugin: &str,
        instance: &str,
        sender_id: &str,
        question_id: &str,
    ) -> Option<GoalId> {
        self.inner.iter().find_map(|e| {
            let meta = e.value().handle_meta.lock().clone();
            if meta.status != AgentRunStatus::Paused {
                return None;
            }
            let origin = meta.origin?;
            if origin.plugin != plugin
                || origin.instance != instance
                || origin.sender_id != sender_id
            {
                return None;
            }
            let snap = (**e.value().snapshot.load()).clone();
            let pending = snap.ask_pending?;
            if pending.question_id == question_id {
                Some(*e.key())
            } else {
                None
            }
        })
    }

    /// Set or clear AskUserQuestion pending state and persist.
    pub async fn set_ask_pending(
        &self,
        goal_id: GoalId,
        pending: Option<AskPendingState>,
    ) -> Result<(), RegistryError> {
        let entry = self
            .inner
            .get(&goal_id)
            .ok_or(RegistryError::NotFound(goal_id))?;
        let mut snap = (**entry.snapshot.load()).clone();
        snap.ask_pending = pending;
        entry.snapshot.store(Arc::new(snap));
        drop(entry);
        self.persist(goal_id).await
    }

    /// Snapshot of every in-flight + queued goal, sorted by start
    /// time descending. Combines memory state with persisted
    /// terminal goals so the consumer sees one unified list.
    pub async fn list(&self) -> Result<Vec<AgentSummary>, RegistryError> {
        let mut out: Vec<AgentSummary> = self
            .inner
            .iter()
            .map(|e| {
                let meta = e.value().handle_meta.lock().clone();
                let snap = (**e.value().snapshot.load()).clone();
                let h = AgentHandle {
                    goal_id: *e.key(),
                    phase_id: meta.phase_id,
                    status: meta.status,
                    origin: meta.origin,
                    dispatcher: meta.dispatcher,
                    started_at: meta.started_at,
                    finished_at: meta.finished_at,
                    snapshot: snap,
                    plan_mode: None,
                    kind: crate::SessionKind::Interactive,
                };
                AgentSummary::from_handle(&h)
            })
            .collect();
        // Add terminal-only rows from store the registry no longer
        // tracks in-memory (e.g. evicted by `release`).
        let persisted = self.store.list().await?;
        for h in persisted {
            if !out.iter().any(|r| r.goal_id == h.goal_id) {
                out.push(AgentSummary::from_handle(&h));
            }
        }
        out.sort_by_key(|b| std::cmp::Reverse(b.wall));
        Ok(out)
    }

    /// Full in-memory handles snapshot. Used by bootstrap routines
    /// that need richer fields than `AgentSummary` exposes.
    pub fn list_handles_in_memory(&self) -> Vec<AgentHandle> {
        self.inner
            .iter()
            .map(|e| {
                let meta = e.value().handle_meta.lock().clone();
                let snap = (**e.value().snapshot.load()).clone();
                AgentHandle {
                    goal_id: *e.key(),
                    phase_id: meta.phase_id,
                    status: meta.status,
                    origin: meta.origin,
                    dispatcher: meta.dispatcher,
                    started_at: meta.started_at,
                    finished_at: meta.finished_at,
                    snapshot: snap,
                    plan_mode: None,
                    kind: crate::SessionKind::Interactive,
                }
            })
            .collect()
    }

    /// Count of `Running` goals — used to enforce the global cap.
    /// Does not count `Queued` or terminal states.
    pub fn count_running(&self) -> u32 {
        self.inner
            .iter()
            .filter(|e| {
                matches!(
                    e.value().handle_meta.lock().status,
                    AgentRunStatus::Running | AgentRunStatus::Sleeping
                )
            })
            .count() as u32
    }

    /// Apply the next `AttemptResult` to the snapshot. Idempotent on
    /// the same `(goal_id, turn_index)` — repeated events from
    /// at-least-once delivery don't double-bump anything.
    pub async fn apply_attempt(&self, ev: &AttemptResult) -> Result<(), RegistryError> {
        let entry = self
            .inner
            .get(&ev.goal_id)
            .ok_or(RegistryError::NotFound(ev.goal_id))?;
        let cur = (**entry.snapshot.load()).clone();
        if ev.turn_index < cur.turn_index {
            // Out-of-order replay — ignore.
            return Ok(());
        }
        let next = AgentSnapshot {
            turn_index: ev.turn_index,
            max_turns: cur.max_turns,
            usage: ev.usage_after.clone(),
            last_acceptance: ev.acceptance.clone(),
            last_decision_summary: summarise_decisions(ev),
            last_event_at: Utc::now(),
            last_diff_stat: ev
                .harness_extras
                .get("worktree.diff_stat")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .or(cur.last_diff_stat),
            last_progress_text: ev.final_text.clone().or(cur.last_progress_text),
            sleep: cur.sleep,
            ask_pending: cur.ask_pending,
        };
        entry.snapshot.store(Arc::new(next));
        self.persist(ev.goal_id).await
    }

    /// Override the snapshot's `max_turns` — called once at admit
    /// time when the orchestrator knows the budget cap. We keep it
    /// out of `admit()` to avoid threading it through callers that
    /// don't have the budget yet.
    pub fn set_max_turns(&self, goal_id: GoalId, max_turns: u32) {
        if let Some(entry) = self.inner.get(&goal_id) {
            let cur = (**entry.snapshot.load()).clone();
            entry
                .snapshot
                .store(Arc::new(AgentSnapshot { max_turns, ..cur }));
        }
    }

    /// Force-update a single field of the snapshot for callers that
    /// learn data outside an `AttemptResult` (e.g. an acceptance
    /// verdict published separately).
    pub fn set_acceptance(&self, goal_id: GoalId, verdict: AcceptanceVerdict) {
        if let Some(entry) = self.inner.get(&goal_id) {
            let cur = (**entry.snapshot.load()).clone();
            entry.snapshot.store(Arc::new(AgentSnapshot {
                last_acceptance: Some(verdict),
                last_event_at: Utc::now(),
                ..cur
            }));
        }
    }

    async fn persist(&self, goal_id: GoalId) -> Result<(), RegistryError> {
        let Some(handle) = self.handle(goal_id) else {
            return Err(RegistryError::NotFound(goal_id));
        };
        self.store.upsert(&handle).await?;
        Ok(())
    }

    fn clear_sleep_snapshot(&self, goal_id: GoalId) {
        if let Some(entry) = self.inner.get(&goal_id) {
            let cur = (**entry.snapshot.load()).clone();
            if cur.sleep.is_some() {
                entry.snapshot.store(Arc::new(AgentSnapshot {
                    sleep: None,
                    last_event_at: Utc::now(),
                    ..cur
                }));
            }
        }
    }
}

fn summarise_decisions(ev: &AttemptResult) -> Option<String> {
    if ev.decisions_recorded.is_empty() {
        return None;
    }
    let last = ev.decisions_recorded.last()?;
    let json = serde_json::to_string(last).ok()?;
    Some(json.chars().take(200).collect())
}
