use std::path::Path;

use async_trait::async_trait;
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
