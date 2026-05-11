//! Consolidation-lock probe contract upstream of
//! nexo-dream and nexo-core.
//!
//! Mirrors the [`AutoDreamHook`] and
//! [`MemoryCheckpointer`] cycle-break pattern: the
//! trait lives here so both `nexo-dream` (provider via
//! `ConsolidationLock`) and `nexo-core` (consumer in dreaming sweep)
//! can depend on it without forming a cycle.
//!
//! Used to coordinate the two-tier memory consolidation in nexo:
//! the **scoring sweep** (light pass, `nexo-core::agent::dreaming`)
//! checks the lock at the start of `run_sweep` and defers when the
//! **fork-pass** (deep, `nexo-dream::AutoDreamRunner`) is currently
//! running. The two writers — scoring sweep and fork — are mutually
//! exclusive per turn: when the fork-pass holds the lock, the
//! scoring sweep defers.
//!
//! [`AutoDreamHook`]: crate::auto_dream::AutoDreamHook
//! [`MemoryCheckpointer`]: crate::memory_checkpoint::MemoryCheckpointer

/// Synchronous probe of an external memory-consolidation lock.
/// Implementors return `true` when a live PID currently holds the
/// lock and the scoring sweep should defer; `false` when the lock
/// is absent, orphaned, or marked rolled-back.
///
/// # Failure semantics
///
/// Implementations MUST NOT panic. They SHOULD fail open — return
/// `false` on transient I/O / parse errors so a probe failure
/// doesn't block the sweep. Real liveness checks log warns inside
/// the impl rather than bubbling errors.
///
/// Sync (not `async`) because real impls are one `stat()` + parse +
/// `kill(0)` — no async I/O surprise.
pub trait ConsolidationLockProbe: Send + Sync + 'static {
    /// `true` iff a live PID is holding the lock right now. Cheap
    /// (one stat + parse + PID check) — safe to call once per turn.
    fn is_live_holder(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Trait-object-safety smoke test: must coerce to
    /// `Arc<dyn ConsolidationLockProbe>` and dispatch through the vtable.
    #[test]
    fn trait_is_object_safe_and_dispatches() {
        struct Toggle(AtomicBool);
        impl ConsolidationLockProbe for Toggle {
            fn is_live_holder(&self) -> bool {
                self.0.load(Ordering::SeqCst)
            }
        }
        let t = Arc::new(Toggle(AtomicBool::new(false)));
        let dyn_ref: Arc<dyn ConsolidationLockProbe> = t.clone();
        assert!(!dyn_ref.is_live_holder());
        t.0.store(true, Ordering::SeqCst);
        assert!(dyn_ref.is_live_holder());
    }
}
