use std::path::Path;

use async_trait::async_trait;
use chrono::Utc;
use nexo_driver_types::{AcceptanceCriterion, AcceptanceVerdict};

use crate::acceptance::AcceptanceEvaluator;
use crate::error::DriverError;

/// Default — every goal that reaches it returns `met=true`. Useful
/// when 67.5's real evaluator is overkill (tests, dev runs).
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
