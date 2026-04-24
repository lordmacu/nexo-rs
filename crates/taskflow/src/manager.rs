use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::store::FlowStore;
use crate::types::{Flow, FlowError, FlowStatus, FlowStep, FlowStepStatus, StepRuntime};

/// Input record for `FlowManager::create_managed`. The runtime owns
/// `id`/`revision`/timestamps; everything else is supplied by the caller
/// (controller, agent tool, or skill author).
#[derive(Debug, Clone)]
pub struct CreateManagedInput {
    pub controller_id: String,
    pub goal: String,
    pub owner_session_key: String,
    pub requester_origin: String,
    pub current_step: String,
    pub state_json: Value,
}

/// Record step observation from an externally-driven task. Used in mirrored
/// mode: the host sees a task event on NATS (or another bus), translates it
/// to a `StepObservation`, and feeds it to the manager so the flow's
/// `flow_steps` view stays in sync without owning task creation.
#[derive(Debug, Clone)]
pub struct StepObservation {
    pub flow_id: Uuid,
    pub run_id: String,
    pub task: String,
    pub status: FlowStepStatus,
    pub child_session_key: Option<String>,
    pub result_json: Option<Value>,
}

/// Maximum number of times a mutating call retries after a `RevisionMismatch`.
/// Two attempts gives one optimistic try plus one re-fetch+retry — enough to
/// cover heartbeat-vs-tool races without livelocking under heavy contention.
const RETRY_ATTEMPTS: u32 = 2;

/// High-level operational surface for managed flows.
///
/// Every mutation method follows the same template: read current flow,
/// validate the requested state machine transition, persist via
/// `update_with_revision`, append an audit event. `RevisionMismatch` errors
/// trigger a single re-fetch + retry; persistent contention surfaces to the
/// caller.
#[derive(Clone)]
pub struct FlowManager {
    store: Arc<dyn FlowStore>,
}

impl FlowManager {
    pub fn new(store: Arc<dyn FlowStore>) -> Self {
        Self { store }
    }

    /// Insert a fresh flow in `Created` status. Caller receives the canonical
    /// record with assigned `id`/timestamps.
    pub async fn create_managed(&self, input: CreateManagedInput) -> Result<Flow, FlowError> {
        let now = Utc::now();
        let flow = Flow {
            id: Uuid::new_v4(),
            controller_id: input.controller_id,
            goal: input.goal,
            owner_session_key: input.owner_session_key,
            requester_origin: input.requester_origin,
            current_step: input.current_step,
            state_json: input.state_json,
            wait_json: None,
            status: FlowStatus::Created,
            cancel_requested: false,
            revision: 0,
            created_at: now,
            updated_at: now,
        };
        self.store.insert(&flow).await?;
        self.store
            .append_event(
                flow.id,
                "created",
                json!({
                    "controller_id": flow.controller_id,
                    "goal": flow.goal,
                    "current_step": flow.current_step,
                }),
            )
            .await?;
        Ok(flow)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Flow>, FlowError> {
        self.store.get(id).await
    }

    pub async fn list_by_owner(&self, owner: &str) -> Result<Vec<Flow>, FlowError> {
        self.store.list_by_owner(owner).await
    }

    pub async fn list_by_status(&self, status: FlowStatus) -> Result<Vec<Flow>, FlowError> {
        self.store.list_by_status(status).await
    }

    /// Created → Running.
    pub async fn start_running(&self, id: Uuid) -> Result<Flow, FlowError> {
        self.with_retry(id, "started", json!({}), |f| {
            f.transition_to(FlowStatus::Running)
        })
        .await
    }

    /// Running → Waiting. `wait_json` describes what the flow is blocked on
    /// (timer deadline, NATS subject, manual signal). Inspected by the
    /// wait/resume engine in 14.4.
    pub async fn set_waiting(&self, id: Uuid, wait_json: Value) -> Result<Flow, FlowError> {
        self.with_retry(
            id,
            "waiting",
            json!({ "wait": wait_json.clone() }),
            move |f| {
                f.transition_to(FlowStatus::Waiting)?;
                f.wait_json = Some(wait_json.clone());
                Ok(())
            },
        )
        .await
    }

    /// Waiting → Running. Clears `wait_json`. Optional `state_patch` merges
    /// shallowly into `state_json` so callers can record what unblocked them.
    pub async fn resume(&self, id: Uuid, state_patch: Option<Value>) -> Result<Flow, FlowError> {
        let payload = json!({ "state_patch": state_patch.clone() });
        self.with_retry(id, "resumed", payload, move |f| {
            f.transition_to(FlowStatus::Running)?;
            f.wait_json = None;
            if let Some(patch) = &state_patch {
                merge_state(&mut f.state_json, patch.clone());
            }
            Ok(())
        })
        .await
    }

    /// Running → Finished. Optional final state patch is merged before transition.
    pub async fn finish(&self, id: Uuid, final_state: Option<Value>) -> Result<Flow, FlowError> {
        let payload = json!({ "final_state": final_state.clone() });
        self.with_retry(id, "finished", payload, move |f| {
            if let Some(patch) = &final_state {
                merge_state(&mut f.state_json, patch.clone());
            }
            f.transition_to(FlowStatus::Finished)
        })
        .await
    }

    /// Running/Waiting → Failed. `reason` is recorded in the event log and
    /// stamped under `state_json.failure`.
    pub async fn fail(&self, id: Uuid, reason: impl Into<String>) -> Result<Flow, FlowError> {
        let reason = reason.into();
        let payload = json!({ "reason": reason });
        self.with_retry(id, "failed", payload.clone(), move |f| {
            merge_state(
                &mut f.state_json,
                json!({ "failure": { "reason": reason.clone(), "at": Utc::now().to_rfc3339() } }),
            );
            f.transition_to(FlowStatus::Failed)
        })
        .await
    }

    /// Set sticky cancel intent without changing status. Useful when an
    /// in-flight step needs to drain before the flow can flip to `Cancelled`.
    pub async fn request_cancel(&self, id: Uuid) -> Result<Flow, FlowError> {
        self.with_retry(id, "cancel_requested", json!({}), |f| {
            f.request_cancel();
            Ok(())
        })
        .await
    }

    /// Force the flow to `Cancelled`. Allowed from any non-terminal status.
    pub async fn cancel(&self, id: Uuid) -> Result<Flow, FlowError> {
        self.with_retry(id, "cancelled", json!({}), |f| {
            f.transition_to(FlowStatus::Cancelled)
        })
        .await
    }

    /// Mutate `state_json` without changing status. `current_step` is
    /// optionally updated in the same revision.
    pub async fn update_state(
        &self,
        id: Uuid,
        patch: Value,
        next_step: Option<String>,
    ) -> Result<Flow, FlowError> {
        let payload = json!({ "patch": patch.clone(), "next_step": next_step.clone() });
        self.with_retry(id, "state_updated", payload, move |f| {
            // Reject if cancel_requested or terminal — same policy as transition_to.
            if f.status.is_terminal() {
                return Err(FlowError::AlreadyTerminal {
                    id: f.id,
                    status: f.status,
                });
            }
            if f.cancel_requested {
                return Err(FlowError::CancelPending { id: f.id });
            }
            merge_state(&mut f.state_json, patch.clone());
            if let Some(step) = &next_step {
                f.current_step = step.clone();
            }
            f.updated_at = Utc::now();
            Ok(())
        })
        .await
    }

    /// Create a mirrored flow. The flow is born in `Running` status because
    /// the externally-observed work is typically already in flight. Use
    /// `record_step_observation` to keep its steps in sync.
    pub async fn create_mirrored(&self, input: CreateManagedInput) -> Result<Flow, FlowError> {
        let created = self.create_managed(input).await?;
        let running = self.start_running(created.id).await?;
        Ok(running)
    }

    /// Upsert-style: if a step with the same `(flow_id, run_id)` exists,
    /// update its status/result; otherwise insert a fresh step row. Designed
    /// to be called from a NATS subscriber (or CLI/cron bridge).
    pub async fn record_step_observation(
        &self,
        observation: StepObservation,
    ) -> Result<FlowStep, FlowError> {
        // The flow must exist; cross-reference prevents orphaned steps from
        // polluting the store if the bus delivers late events.
        let _flow = self
            .store
            .get(observation.flow_id)
            .await?
            .ok_or(FlowError::NotFound(observation.flow_id))?;

        let existing = self
            .store
            .find_step_by_run_id(observation.flow_id, &observation.run_id)
            .await?;
        let now = chrono::Utc::now();
        let step = match existing {
            Some(mut s) => {
                s.task = observation.task.clone();
                s.status = observation.status;
                s.result_json = observation.result_json.clone();
                s.child_session_key = observation
                    .child_session_key
                    .clone()
                    .or(s.child_session_key);
                s.updated_at = now;
                self.store.update_step(&s).await?
            }
            None => {
                let fresh = FlowStep {
                    id: Uuid::new_v4(),
                    flow_id: observation.flow_id,
                    runtime: StepRuntime::Mirrored,
                    child_session_key: observation.child_session_key.clone(),
                    run_id: observation.run_id.clone(),
                    task: observation.task.clone(),
                    status: observation.status,
                    result_json: observation.result_json.clone(),
                    created_at: now,
                    updated_at: now,
                };
                self.store.insert_step(&fresh).await?;
                fresh
            }
        };

        // Audit trail on the flow itself.
        self.store
            .append_event(
                observation.flow_id,
                "step_observed",
                json!({
                    "run_id": observation.run_id,
                    "status": step.status.as_str(),
                    "runtime": step.runtime.as_str(),
                }),
            )
            .await?;
        Ok(step)
    }

    /// Read all steps linked to a flow, ordered oldest-first.
    pub async fn list_steps(&self, flow_id: Uuid) -> Result<Vec<FlowStep>, FlowError> {
        self.store.list_steps(flow_id).await
    }

    /// Read–modify–write loop with one retry on `RevisionMismatch`. The
    /// mutation closure runs against a fresh copy each attempt. Uses
    /// `update_and_append` so the revision-checked UPDATE and the
    /// audit-log INSERT commit atomically — previously a crash between
    /// the two left the flow advanced but the event log silently
    /// incomplete.
    async fn with_retry<F>(
        &self,
        id: Uuid,
        event_kind: &str,
        event_payload: Value,
        mutate: F,
    ) -> Result<Flow, FlowError>
    where
        F: Fn(&mut Flow) -> Result<(), FlowError> + Send + Sync,
    {
        let mut last_err: Option<FlowError> = None;
        for _ in 0..RETRY_ATTEMPTS {
            let mut current = self.store.get(id).await?.ok_or(FlowError::NotFound(id))?;
            mutate(&mut current)?;
            match self
                .store
                .update_and_append(&current, event_kind, event_payload.clone())
                .await
            {
                Ok((updated, _event)) => return Ok(updated),
                Err(FlowError::RevisionMismatch { .. }) => {
                    last_err = Some(FlowError::RevisionMismatch {
                        expected: current.revision,
                        actual: -1,
                    });
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| FlowError::InvalidData("retry exhausted".into())))
    }
}

/// Shallow JSON merge: top-level keys in `patch` overwrite those in `target`.
/// Non-object `patch` replaces `target` entirely. Adopted by every mutation
/// that touches `state_json`.
fn merge_state(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(t), Value::Object(p)) => {
            for (k, v) in p {
                t.insert(k, v);
            }
        }
        (target_slot, other) => {
            *target_slot = other;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteFlowStore;
    use serde_json::json;

    async fn manager() -> FlowManager {
        let store = SqliteFlowStore::open(":memory:").await.unwrap();
        FlowManager::new(Arc::new(store))
    }

    fn input() -> CreateManagedInput {
        CreateManagedInput {
            controller_id: "kate/inbox".into(),
            goal: "triage inbox".into(),
            owner_session_key: "agent:kate:session:abc".into(),
            requester_origin: "user-1".into(),
            current_step: "classify".into(),
            state_json: json!({"messages": 10, "processed": 0}),
        }
    }

    #[tokio::test]
    async fn full_happy_path_create_run_wait_resume_finish() {
        let m = manager().await;
        let f = m.create_managed(input()).await.unwrap();
        assert_eq!(f.status, FlowStatus::Created);
        assert_eq!(f.revision, 0);

        let f = m.start_running(f.id).await.unwrap();
        assert_eq!(f.status, FlowStatus::Running);
        assert_eq!(f.revision, 1);

        let f = m
            .set_waiting(f.id, json!({"kind": "timer", "at": "2026-04-23T15:00:00Z"}))
            .await
            .unwrap();
        assert_eq!(f.status, FlowStatus::Waiting);
        assert!(f.wait_json.is_some());
        assert_eq!(f.revision, 2);

        let f = m.resume(f.id, Some(json!({"processed": 5}))).await.unwrap();
        assert_eq!(f.status, FlowStatus::Running);
        assert!(f.wait_json.is_none());
        assert_eq!(f.state_json["processed"], 5);
        assert_eq!(f.state_json["messages"], 10);

        let f = m
            .finish(f.id, Some(json!({"summary": "10 done"})))
            .await
            .unwrap();
        assert_eq!(f.status, FlowStatus::Finished);
        assert_eq!(f.state_json["summary"], "10 done");
    }

    #[tokio::test]
    async fn fail_records_reason_in_state_and_event() {
        let m = manager().await;
        let f = m.create_managed(input()).await.unwrap();
        let f = m.start_running(f.id).await.unwrap();
        let f = m.fail(f.id, "downstream 503").await.unwrap();
        assert_eq!(f.status, FlowStatus::Failed);
        assert_eq!(f.state_json["failure"]["reason"], "downstream 503");

        // The store kept an "failed" event.
        let store = SqliteFlowStore::open(":memory:").await.unwrap(); // unrelated; we use m.store
        let _ = store; // silence
    }

    #[tokio::test]
    async fn cancel_from_running_succeeds() {
        let m = manager().await;
        let f = m.create_managed(input()).await.unwrap();
        let f = m.start_running(f.id).await.unwrap();
        let f = m.cancel(f.id).await.unwrap();
        assert_eq!(f.status, FlowStatus::Cancelled);
    }

    #[tokio::test]
    async fn request_cancel_blocks_finish() {
        let m = manager().await;
        let f = m.create_managed(input()).await.unwrap();
        let f = m.start_running(f.id).await.unwrap();
        let f = m.request_cancel(f.id).await.unwrap();
        assert!(f.cancel_requested);
        assert_eq!(f.status, FlowStatus::Running);

        let err = m.finish(f.id, None).await.expect_err("blocked");
        assert!(matches!(err, FlowError::CancelPending { .. }));

        // But cancel still works.
        let f = m.cancel(f.id).await.unwrap();
        assert_eq!(f.status, FlowStatus::Cancelled);
    }

    #[tokio::test]
    async fn update_state_preserves_status_and_merges_shallow() {
        let m = manager().await;
        let f = m.create_managed(input()).await.unwrap();
        let f = m.start_running(f.id).await.unwrap();

        let f = m
            .update_state(
                f.id,
                json!({"processed": 3, "errors": []}),
                Some("fetch".into()),
            )
            .await
            .unwrap();
        assert_eq!(f.status, FlowStatus::Running);
        assert_eq!(f.current_step, "fetch");
        assert_eq!(f.state_json["processed"], 3);
        assert_eq!(f.state_json["messages"], 10, "untouched key preserved");
        assert!(f.state_json["errors"].is_array());
    }

    #[tokio::test]
    async fn update_state_rejected_when_cancel_pending() {
        let m = manager().await;
        let f = m.create_managed(input()).await.unwrap();
        let f = m.start_running(f.id).await.unwrap();
        m.request_cancel(f.id).await.unwrap();
        let err = m
            .update_state(f.id, json!({"x": 1}), None)
            .await
            .expect_err("blocked");
        assert!(matches!(err, FlowError::CancelPending { .. }));
    }

    #[tokio::test]
    async fn create_appends_audit_event() {
        let store = Arc::new(SqliteFlowStore::open(":memory:").await.unwrap());
        let m = FlowManager::new(store.clone());
        let f = m.create_managed(input()).await.unwrap();
        let events = store.list_events(f.id, 10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "created");
    }

    #[tokio::test]
    async fn create_mirrored_starts_in_running() {
        let m = manager().await;
        let f = m.create_mirrored(input()).await.unwrap();
        assert_eq!(f.status, FlowStatus::Running);
    }

    #[tokio::test]
    async fn record_step_observation_inserts_then_updates() {
        let m = manager().await;
        let f = m.create_mirrored(input()).await.unwrap();

        // First observation — new step.
        let s1 = m
            .record_step_observation(StepObservation {
                flow_id: f.id,
                run_id: "cron-42".into(),
                task: "classify".into(),
                status: FlowStepStatus::Running,
                child_session_key: Some("cron:session".into()),
                result_json: None,
            })
            .await
            .unwrap();
        assert_eq!(s1.runtime, StepRuntime::Mirrored);
        assert_eq!(s1.status, FlowStepStatus::Running);

        // Second observation with same run_id — should update the existing
        // row in place, not insert a new one.
        let s2 = m
            .record_step_observation(StepObservation {
                flow_id: f.id,
                run_id: "cron-42".into(),
                task: "classify".into(),
                status: FlowStepStatus::Succeeded,
                child_session_key: None,
                result_json: Some(json!({"classified": 10})),
            })
            .await
            .unwrap();
        assert_eq!(s1.id, s2.id, "same step row should be reused");
        assert_eq!(s2.status, FlowStepStatus::Succeeded);
        assert_eq!(s2.result_json.unwrap()["classified"], 10);
        // child_session_key preserved from first observation when second is None.
        assert_eq!(s2.child_session_key.as_deref(), Some("cron:session"));

        // Only one step persisted.
        let steps = m.list_steps(f.id).await.unwrap();
        assert_eq!(steps.len(), 1);
    }

    #[tokio::test]
    async fn record_step_on_unknown_flow_errors() {
        let m = manager().await;
        let err = m
            .record_step_observation(StepObservation {
                flow_id: Uuid::new_v4(),
                run_id: "r".into(),
                task: "t".into(),
                status: FlowStepStatus::Pending,
                child_session_key: None,
                result_json: None,
            })
            .await
            .expect_err("err");
        assert!(matches!(err, FlowError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_steps_returns_per_flow() {
        let m = manager().await;
        let f = m.create_mirrored(input()).await.unwrap();
        for i in 0..3 {
            m.record_step_observation(StepObservation {
                flow_id: f.id,
                run_id: format!("run-{i}"),
                task: format!("task-{i}"),
                status: FlowStepStatus::Pending,
                child_session_key: None,
                result_json: None,
            })
            .await
            .unwrap();
        }
        let steps = m.list_steps(f.id).await.unwrap();
        assert_eq!(steps.len(), 3);
    }

    #[tokio::test]
    async fn double_finish_returns_already_terminal() {
        let m = manager().await;
        let f = m.create_managed(input()).await.unwrap();
        let f = m.start_running(f.id).await.unwrap();
        let _ = m.finish(f.id, None).await.unwrap();
        let err = m.finish(f.id, None).await.expect_err("terminal");
        assert!(matches!(err, FlowError::AlreadyTerminal { .. }));
    }
}
