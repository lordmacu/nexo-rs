//! `PermissionDecider` trait + the test impls 67.3 ships.
//! 67.4 will add `LlmDecider` consulting MiniMax + memory.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::PermissionError;
use crate::types::{PermissionOutcome, PermissionRequest, PermissionResponse};

#[async_trait]
pub trait PermissionDecider: Send + Sync + 'static {
    async fn decide(
        &self,
        request: PermissionRequest,
    ) -> Result<PermissionResponse, PermissionError>;
}

/// Allows every request. **DEV ONLY** — the bin emits a loud
/// `tracing::warn!` when it boots with this decider so an operator
/// who ran `nexo-driver-permission-mcp` standalone notices.
#[derive(Default)]
pub struct AllowAllDecider;

#[async_trait]
impl PermissionDecider for AllowAllDecider {
    async fn decide(
        &self,
        request: PermissionRequest,
    ) -> Result<PermissionResponse, PermissionError> {
        Ok(PermissionResponse {
            tool_use_id: request.tool_use_id,
            outcome: PermissionOutcome::AllowOnce {
                updated_input: None,
            },
            rationale: "AllowAllDecider — testing/dev only".into(),
        })
    }
}

/// Denies every request with `reason`. Useful for shadow-mode
/// (Phase 67.11) where Claude proposes but never acts.
pub struct DenyAllDecider {
    pub reason: String,
}

#[async_trait]
impl PermissionDecider for DenyAllDecider {
    async fn decide(
        &self,
        request: PermissionRequest,
    ) -> Result<PermissionResponse, PermissionError> {
        Ok(PermissionResponse {
            tool_use_id: request.tool_use_id,
            outcome: PermissionOutcome::Deny {
                message: self.reason.clone(),
            },
            rationale: format!("DenyAllDecider: {}", self.reason),
        })
    }
}

/// Returns scripted outcomes in order. `decide` errors with
/// `PermissionError::Decider("ScriptedDecider exhausted")` if the
/// queue runs dry — keeps tests honest about how many calls they
/// expect.
pub struct ScriptedDecider {
    queue: Mutex<VecDeque<PermissionOutcome>>,
}

impl ScriptedDecider {
    pub fn new<I>(items: I) -> Self
    where
        I: IntoIterator<Item = PermissionOutcome>,
    {
        Self {
            queue: Mutex::new(items.into_iter().collect()),
        }
    }
}

#[async_trait]
impl PermissionDecider for ScriptedDecider {
    async fn decide(
        &self,
        request: PermissionRequest,
    ) -> Result<PermissionResponse, PermissionError> {
        let outcome = self
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front()
            .ok_or_else(|| PermissionError::Decider("ScriptedDecider exhausted".into()))?;
        Ok(PermissionResponse {
            tool_use_id: request.tool_use_id,
            outcome,
            rationale: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AllowScope;

    fn req() -> PermissionRequest {
        PermissionRequest {
            goal_id: nexo_driver_types::GoalId::new(),
            tool_use_id: "tu_test".into(),
            tool_name: "Edit".into(),
            input: serde_json::json!({}),
            metadata: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn scripted_iterates_in_order() {
        let d = ScriptedDecider::new([
            PermissionOutcome::AllowOnce {
                updated_input: None,
            },
            PermissionOutcome::Deny {
                message: "no".into(),
            },
            PermissionOutcome::AllowSession {
                scope: AllowScope::Session,
                updated_input: None,
            },
        ]);
        let r1 = d.decide(req()).await.unwrap();
        let r2 = d.decide(req()).await.unwrap();
        let r3 = d.decide(req()).await.unwrap();
        assert!(matches!(r1.outcome, PermissionOutcome::AllowOnce { .. }));
        assert!(matches!(r2.outcome, PermissionOutcome::Deny { .. }));
        assert!(matches!(r3.outcome, PermissionOutcome::AllowSession { .. }));
    }

    #[tokio::test]
    async fn scripted_exhausted_returns_decider_error() {
        let d = ScriptedDecider::new([]);
        let err = d.decide(req()).await.unwrap_err();
        assert!(matches!(err, PermissionError::Decider(_)));
    }
}
