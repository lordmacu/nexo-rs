//! `AcceptanceEvaluator` trait. Phase 67.4 ships only `Noop`; 67.5
//! implements the real shell-command / file-match / custom verifier
//! dispatcher.

use std::path::Path;

use async_trait::async_trait;
use chrono::Utc;
use nexo_driver_types::{AcceptanceCriterion, AcceptanceVerdict};

use crate::error::DriverError;

#[async_trait]
pub trait AcceptanceEvaluator: Send + Sync + 'static {
    async fn evaluate(
        &self,
        criteria: &[AcceptanceCriterion],
        workspace: &Path,
    ) -> Result<AcceptanceVerdict, DriverError>;
}

/// Default — every goal that reaches it returns `met=true`. Useful
/// for 67.4 tests that orchestrate without 67.5's real evaluator.
#[derive(Default)]
pub struct NoopAcceptanceEvaluator;

#[async_trait]
impl AcceptanceEvaluator for NoopAcceptanceEvaluator {
    async fn evaluate(
        &self,
        _criteria: &[AcceptanceCriterion],
        _workspace: &Path,
    ) -> Result<AcceptanceVerdict, DriverError> {
        Ok(AcceptanceVerdict {
            met: true,
            failures: vec![],
            evaluated_at: Utc::now(),
            elapsed_ms: 0,
        })
    }
}
