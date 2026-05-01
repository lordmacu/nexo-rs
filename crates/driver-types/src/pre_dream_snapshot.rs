//! Pre-dream snapshot contract.
//!
//! `nexo-dream`'s `AutoDreamRunner` consults this trait, when wired,
//! to capture a point-in-time bundle of the agent's memory immediately
//! before the fork-pass mutates anything. The default implementation
//! (`nexo-memory-snapshot::PreDreamSnapshotHookAdapter`) calls into
//! `MemorySnapshotter::snapshot` with `created_by = "auto-pre-dream"`.
//!
//! Provider-agnostic by design: the trait knows nothing about LLMs,
//! brokers, or storage backends — those decisions live in the
//! adapter the operator wires at boot.

use async_trait::async_trait;

/// Sink for pre-dream snapshots. Implementations must be best-effort:
/// a snapshot failure logs `tracing::warn!` in the consumer and the
/// dream proceeds without the rollback anchor. Operators who want a
/// hard "no dream without snapshot" gate enforce that at the boot
/// wire, not here.
#[async_trait]
pub trait PreDreamSnapshotHook: Send + Sync + 'static {
    /// Capture the snapshot. Implementations choose the label from
    /// the `run_id` so the resulting bundle is correlatable to the
    /// dream-run audit row.
    ///
    /// `agent_id` is the canonical id from `agents.yaml`; `tenant`
    /// is the multi-tenant scope (`"default"` for single-tenant
    /// deployments). `run_id` correlates the snapshot bundle to the
    /// dream-run audit row in `dream_runs.db`.
    async fn snapshot_before_dream(
        &self,
        agent_id: &str,
        tenant: &str,
        run_id: &str,
    ) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn trait_is_object_safe_and_dispatches() {
        struct Counter(AtomicUsize);
        #[async_trait]
        impl PreDreamSnapshotHook for Counter {
            async fn snapshot_before_dream(
                &self,
                _agent_id: &str,
                _tenant: &str,
                _run_id: &str,
            ) -> Result<(), String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
        let counter = Arc::new(Counter(AtomicUsize::new(0)));
        let dyn_ref: Arc<dyn PreDreamSnapshotHook> = counter.clone();
        dyn_ref
            .snapshot_before_dream("ana", "default", "run-1")
            .await
            .unwrap();
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn impl_can_return_error_string() {
        struct Failing;
        #[async_trait]
        impl PreDreamSnapshotHook for Failing {
            async fn snapshot_before_dream(
                &self,
                _agent_id: &str,
                _tenant: &str,
                _run_id: &str,
            ) -> Result<(), String> {
                Err("simulated".into())
            }
        }
        let f = Failing;
        let err = f.snapshot_before_dream("ana", "default", "x").await;
        assert!(err.is_err());
    }
}
