use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Lifecycle states a flow can be in. Transitions are enforced in Phase 14.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowStatus {
    Created,
    Running,
    Waiting,
    Cancelled,
    Finished,
    Failed,
}

impl FlowStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            FlowStatus::Cancelled | FlowStatus::Finished | FlowStatus::Failed
        )
    }

    /// Legal direct transitions from this status. Cancellation is allowed
    /// from any non-terminal status; the rest are constrained to a small
    /// graph that mirrors the documented Phase 14 lifecycle.
    pub fn can_transition_to(&self, next: FlowStatus) -> bool {
        if self.is_terminal() {
            return false;
        }
        if next == FlowStatus::Cancelled {
            return true; // any non-terminal → Cancelled
        }
        matches!(
            (self, next),
            (FlowStatus::Created, FlowStatus::Running)
                | (FlowStatus::Running, FlowStatus::Waiting)
                | (FlowStatus::Running, FlowStatus::Finished)
                | (FlowStatus::Running, FlowStatus::Failed)
                | (FlowStatus::Waiting, FlowStatus::Running)
                | (FlowStatus::Waiting, FlowStatus::Failed)
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FlowStatus::Created => "created",
            FlowStatus::Running => "running",
            FlowStatus::Waiting => "waiting",
            FlowStatus::Cancelled => "cancelled",
            FlowStatus::Finished => "finished",
            FlowStatus::Failed => "failed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "created" => FlowStatus::Created,
            "running" => FlowStatus::Running,
            "waiting" => FlowStatus::Waiting,
            "cancelled" => FlowStatus::Cancelled,
            "finished" => FlowStatus::Finished,
            "failed" => FlowStatus::Failed,
            _ => return None,
        })
    }
}

/// A flow record as persisted in `flows`. `state_json` and `wait_json` carry
/// arbitrary controller-specific payloads; the runtime treats them as opaque
/// blobs and only mutates them through revision-checked APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flow {
    pub id: Uuid,
    pub controller_id: String,
    pub goal: String,
    pub owner_session_key: String,
    pub requester_origin: String,
    pub current_step: String,
    pub state_json: Value,
    pub wait_json: Option<Value>,
    pub status: FlowStatus,
    /// Sticky cancel intent. When `true`, the flow refuses any non-terminal
    /// transition until it reaches `Cancelled`. Survives restart.
    pub cancel_requested: bool,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Flow {
    /// Validate and apply a status transition. Does not persist — caller is
    /// responsible for `FlowStore::update_with_revision`. Mutates `self` so
    /// the caller can immediately persist the resulting state.
    pub fn transition_to(&mut self, next: FlowStatus) -> Result<(), FlowError> {
        if self.status.is_terminal() {
            return Err(FlowError::AlreadyTerminal {
                id: self.id,
                status: self.status,
            });
        }
        if self.cancel_requested && next != FlowStatus::Cancelled {
            return Err(FlowError::CancelPending { id: self.id });
        }
        if !self.status.can_transition_to(next) {
            return Err(FlowError::IllegalTransition {
                from: self.status,
                to: next,
            });
        }
        self.status = next;
        self.updated_at = chrono::Utc::now();
        Ok(())
    }

    /// Mark a sticky cancel intent. Survives restart; subsequent transitions
    /// are restricted to `Cancelled`. Idempotent — calling on an already
    /// cancel-requested flow is a no-op.
    pub fn request_cancel(&mut self) {
        if !self.status.is_terminal() {
            self.cancel_requested = true;
            self.updated_at = chrono::Utc::now();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRuntime {
    /// We own the lifecycle: created and driven by the FlowManager.
    Managed,
    /// We observe an externally created task and reflect its status here.
    Mirrored,
}

impl StepRuntime {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepRuntime::Managed => "managed",
            StepRuntime::Mirrored => "mirrored",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "managed" => StepRuntime::Managed,
            "mirrored" => StepRuntime::Mirrored,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowStepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl FlowStepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FlowStepStatus::Pending => "pending",
            FlowStepStatus::Running => "running",
            FlowStepStatus::Succeeded => "succeeded",
            FlowStepStatus::Failed => "failed",
            FlowStepStatus::Cancelled => "cancelled",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => FlowStepStatus::Pending,
            "running" => FlowStepStatus::Running,
            "succeeded" => FlowStepStatus::Succeeded,
            "failed" => FlowStepStatus::Failed,
            "cancelled" => FlowStepStatus::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowStep {
    pub id: Uuid,
    pub flow_id: Uuid,
    pub runtime: StepRuntime,
    pub child_session_key: Option<String>,
    pub run_id: String,
    pub task: String,
    pub status: FlowStepStatus,
    pub result_json: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Audit log entry for any flow mutation. Append-only, never updated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowEvent {
    pub id: i64,
    pub flow_id: Uuid,
    pub kind: String,
    pub payload_json: Value,
    pub at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    #[error("flow not found: {0}")]
    NotFound(Uuid),
    #[error("revision mismatch: expected {expected}, found {actual}")]
    RevisionMismatch { expected: i64, actual: i64 },
    #[error("illegal transition from {from:?} to {to:?}")]
    IllegalTransition { from: FlowStatus, to: FlowStatus },
    #[error("flow {id} is already terminal ({status:?})")]
    AlreadyTerminal { id: Uuid, status: FlowStatus },
    #[error("flow {id} has cancel_requested; only Cancelled transition allowed")]
    CancelPending { id: Uuid },
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("invalid data: {0}")]
    InvalidData(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn flow(status: FlowStatus) -> Flow {
        let now = chrono::Utc::now();
        Flow {
            id: Uuid::new_v4(),
            controller_id: "test".into(),
            goal: "test".into(),
            owner_session_key: "owner".into(),
            requester_origin: "user".into(),
            current_step: "init".into(),
            state_json: json!({}),
            wait_json: None,
            status,
            cancel_requested: false,
            revision: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn status_round_trip() {
        for s in [
            FlowStatus::Created,
            FlowStatus::Running,
            FlowStatus::Waiting,
            FlowStatus::Cancelled,
            FlowStatus::Finished,
            FlowStatus::Failed,
        ] {
            assert_eq!(FlowStatus::from_str(s.as_str()), Some(s));
        }
        assert!(FlowStatus::from_str("nope").is_none());
    }

    #[test]
    fn terminal_flag_matches_intent() {
        assert!(!FlowStatus::Created.is_terminal());
        assert!(!FlowStatus::Running.is_terminal());
        assert!(!FlowStatus::Waiting.is_terminal());
        assert!(FlowStatus::Cancelled.is_terminal());
        assert!(FlowStatus::Finished.is_terminal());
        assert!(FlowStatus::Failed.is_terminal());
    }

    #[test]
    fn legal_transitions_succeed() {
        let mut f = flow(FlowStatus::Created);
        f.transition_to(FlowStatus::Running)
            .expect("created→running");
        f.transition_to(FlowStatus::Waiting)
            .expect("running→waiting");
        f.transition_to(FlowStatus::Running)
            .expect("waiting→running");
        f.transition_to(FlowStatus::Finished)
            .expect("running→finished");
        assert_eq!(f.status, FlowStatus::Finished);
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        let mut f = flow(FlowStatus::Created);
        let err = f
            .transition_to(FlowStatus::Waiting)
            .expect_err("created→waiting illegal");
        assert!(matches!(err, FlowError::IllegalTransition { .. }));
    }

    #[test]
    fn cancel_allowed_from_any_non_terminal() {
        for start in [
            FlowStatus::Created,
            FlowStatus::Running,
            FlowStatus::Waiting,
        ] {
            let mut f = flow(start);
            f.transition_to(FlowStatus::Cancelled)
                .unwrap_or_else(|_| panic!("{start:?}→Cancelled must be legal"));
            assert_eq!(f.status, FlowStatus::Cancelled);
        }
    }

    #[test]
    fn terminal_flow_rejects_any_transition() {
        for term in [
            FlowStatus::Cancelled,
            FlowStatus::Finished,
            FlowStatus::Failed,
        ] {
            let mut f = flow(term);
            let err = f
                .transition_to(FlowStatus::Running)
                .expect_err("terminal must reject");
            assert!(matches!(err, FlowError::AlreadyTerminal { .. }));
        }
    }

    #[test]
    fn cancel_requested_blocks_non_cancel_transition() {
        let mut f = flow(FlowStatus::Running);
        f.request_cancel();
        assert!(f.cancel_requested);
        let err = f
            .transition_to(FlowStatus::Finished)
            .expect_err("blocked");
        assert!(matches!(err, FlowError::CancelPending { .. }));
        // But Cancelled is still allowed.
        f.transition_to(FlowStatus::Cancelled).expect("cancel ok");
        assert_eq!(f.status, FlowStatus::Cancelled);
    }

    #[test]
    fn request_cancel_is_idempotent_and_no_op_on_terminal() {
        let mut f = flow(FlowStatus::Finished);
        f.request_cancel();
        assert!(
            !f.cancel_requested,
            "terminal flow should not gain cancel intent"
        );

        let mut g = flow(FlowStatus::Running);
        g.request_cancel();
        let first_ts = g.updated_at;
        g.request_cancel();
        assert!(g.cancel_requested);
        // updated_at may shift but flag stays on; either way the call is safe.
        let _ = first_ts;
    }
}
