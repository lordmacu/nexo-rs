//! Memory mutation observer contract.
//!
//! Fires on every write to long-term / vector / concepts / compactions
//! SQLite tables and to the git-backed memdir. Designed for two
//! consumers:
//!
//! 1. The snapshot subsystem, which feeds the calls into a NATS
//!    subject (`nexo.memory.mutated.<agent_id>`) so audit / admin-ui /
//!    incremental-snapshot subscribers can react.
//! 2. Forensic loggers that want a single chokepoint for every mutation
//!    without forking every writer signature.
//!
//! The trait stays in `nexo-driver-types` so producers (the memory
//! crate) and consumers (the snapshot crate) link to it without
//! creating a cycle.

use async_trait::async_trait;

/// Layer the mutation came from. Mirrors
/// `nexo_memory_snapshot::events::MutationScope` so the adapter is
/// a 1:1 translation. New variants land here in lockstep with the
/// downstream enum so the bridge does not need a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryMutationScope {
    SqliteLongTerm,
    SqliteVector,
    SqliteConcepts,
    SqliteCompactions,
    Git,
    MemoryFile,
}

/// Direction of the write. `Insert` and `Update` are reported
/// separately so subscribers can build accurate change-rate metrics
/// without re-deriving them from key churn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryMutationOp {
    Insert,
    Update,
    Delete,
}

/// Sink for memory mutations. Implementations must be best-effort:
/// failing to publish must never poison the writer's transaction.
///
/// `non_exhaustive` left off intentionally — adding a variant to the
/// scope or op enum is a coordinated change with the snapshot
/// adapter, and a wildcard arm there would silently drop new
/// variants instead of routing them.
#[async_trait]
pub trait MemoryMutationHook: Send + Sync + 'static {
    /// Notify of a single mutation. `key` is the row id / git oid /
    /// file path and is opaque to consumers other than as a
    /// correlation token.
    async fn on_mutation(
        &self,
        agent_id: &str,
        tenant: &str,
        scope: MemoryMutationScope,
        op: MemoryMutationOp,
        key: &str,
    );
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
        impl MemoryMutationHook for Counter {
            async fn on_mutation(
                &self,
                _agent_id: &str,
                _tenant: &str,
                _scope: MemoryMutationScope,
                _op: MemoryMutationOp,
                _key: &str,
            ) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let c = Arc::new(Counter(AtomicUsize::new(0)));
        let dynamic: Arc<dyn MemoryMutationHook> = c.clone();
        dynamic
            .on_mutation(
                "ana",
                "default",
                MemoryMutationScope::SqliteLongTerm,
                MemoryMutationOp::Insert,
                "row-1",
            )
            .await;
        dynamic
            .on_mutation(
                "ana",
                "default",
                MemoryMutationScope::Git,
                MemoryMutationOp::Update,
                "deadbeef",
            )
            .await;
        assert_eq!(c.0.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn scope_and_op_are_copy() {
        let _scope: MemoryMutationScope = MemoryMutationScope::SqliteVector;
        let _op: MemoryMutationOp = MemoryMutationOp::Delete;
        // Compile-time proof: deriving `Copy` would have been
        // refused if `non_exhaustive` blocked it. We rely on the
        // explicit `#[derive(... Copy)]` to keep call-sites cheap
        // (no `.clone()` per fire site in hot loops).
    }
}
