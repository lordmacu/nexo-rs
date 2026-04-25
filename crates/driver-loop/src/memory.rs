//! `DecisionMemory` trait — semantic recall hook for the LlmDecider.
//! Phase 67.4 ships `Noop`; 67.7 wires sqlite-vec.

use async_trait::async_trait;
use nexo_driver_permission::PermissionRequest;
use nexo_driver_types::Decision;

#[async_trait]
pub trait DecisionMemory: Send + Sync + 'static {
    /// Up to `k` past decisions semantically similar to `req`. Order
    /// is implementation-defined; impls should put strongest matches
    /// first.
    async fn recall(&self, req: &PermissionRequest, k: usize) -> Vec<Decision>;
}

#[derive(Default)]
pub struct NoopDecisionMemory;

#[async_trait]
impl DecisionMemory for NoopDecisionMemory {
    async fn recall(&self, _req: &PermissionRequest, _k: usize) -> Vec<Decision> {
        Vec::new()
    }
}
