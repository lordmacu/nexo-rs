//! Memory-checkpoint contract upstream of nexo-dream and nexo-core.
//!
//! Mirrors the [`AutoDreamHook`] pattern: the trait
//! lives here (in the low-level types crate) so both `nexo-dream`
//! (consumer) and `nexo-core` (provider via `MemoryGitRepo`) can
//! depend on `nexo-driver-types` without forming a cycle.
//!
//! Provider-agnostic by construction: any storage backend (git,
//! object-store, dual-write audit log) can implement this trait. The
//! LLM provider does not enter the decision — checkpoints are pure
//! infrastructure-layer artifacts.
//!
//! The concrete `nexo-core` implementation builds on
//! `crates/core/src/agent/workspace_git.rs`'s `MemoryGitRepo`, which
//! enforces a secret-guard + `MAX_COMMIT_FILE_BYTES` cap; this trait
//! extends the same git-backed memory pattern to the fork-pass deep
//! consolidation.
//!
//! [`AutoDreamHook`]: crate::auto_dream::AutoDreamHook

use async_trait::async_trait;

/// Sink for memory-state checkpoints. Implemented by
/// `nexo_core::agent::MemoryGitRepo` (git-backed memory);
/// called by nexo-dream's `AutoDreamRunner` after a successful
/// fork-pass to record the resulting `memory_dir` state.
///
/// # Failure semantics
///
/// Implementations MUST NOT panic. Return an `Err(String)` (the
/// runner logs it as `tracing::warn!` and continues — the audit row
/// in `dream_runs.db` is the source of truth, the checkpoint is
/// bonus forensics).
#[async_trait]
pub trait MemoryCheckpointer: Send + Sync + 'static {
    /// Record a checkpoint. `subject` is short (≤ 50 chars
    /// recommended); `body` is freeform markdown. Both are owned
    /// `String`s so the impl can safely `move` them into a
    /// `spawn_blocking` closure.
    async fn checkpoint(&self, subject: String, body: String) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Trait-object-safety smoke test: must coerce to
    /// `Arc<dyn MemoryCheckpointer>` and dispatch through the vtable.
    #[tokio::test]
    async fn trait_is_object_safe_and_dispatches() {
        struct Counter(AtomicUsize);
        #[async_trait]
        impl MemoryCheckpointer for Counter {
            async fn checkpoint(&self, _subject: String, _body: String) -> Result<(), String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
        let counter = Arc::new(Counter(AtomicUsize::new(0)));
        let dyn_ref: Arc<dyn MemoryCheckpointer> = counter.clone();
        dyn_ref
            .checkpoint("subj".into(), "body".into())
            .await
            .unwrap();
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    }
}
