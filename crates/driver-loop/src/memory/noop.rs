use async_trait::async_trait;
use nexo_driver_permission::PermissionRequest;
use nexo_driver_types::Decision;

use crate::memory::trait_def::DecisionMemory;

#[derive(Default)]
pub struct NoopDecisionMemory;

#[async_trait]
impl DecisionMemory for NoopDecisionMemory {
    async fn recall(&self, _req: &PermissionRequest, _k: usize) -> Vec<Decision> {
        Vec::new()
    }
}
