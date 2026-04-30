//! Phase 80.1.g — memory-checkpoint contract upstream of nexo-dream
//! and nexo-core.
//!
//! Mirrors the [`AutoDreamHook`] pattern from Phase 80.1.b: the trait
//! lives here (in the low-level types crate) so both `nexo-dream`
//! (consumer) and `nexo-core` (provider via `MemoryGitRepo`) can
//! depend on `nexo-driver-types` without forming a cycle.
//!
//! Provider-agnostic by construction: any storage backend (git,
//! object-store, dual-write audit log) can implement this trait. The
//! LLM provider does not enter the decision — checkpoints are pure
//! infrastructure-layer artifacts.
//!
//! [`AutoDreamHook`]: crate::auto_dream::AutoDreamHook
//!
//! # Reference
//!
//! - **claude-code-leak (PRIMARY)**: NO autoDream→git wiring in the
//!   leak. `claude-code-leak/src/memdir/paths.ts:14` uses
//!   `findCanonicalGitRoot` only to locate the memory dir, not to
//!   commit. `memoryTypes.ts:187` documents the leak's stance: "Git
//!   history, recent changes, or who-changed-what — `git log` /
//!   `git blame` are authoritative" — the leak does NOT duplicate
//!   git info into memory. Phase 10.9 git-backed memory is a
//!   nexo-specific innovation; this trait extends it to the
//!   fork-pass deep consolidation.
//! - **research/ (OpenClaw)**: no git-as-memory pattern; absence
//!   noted. Single-process Node app expects user to manage git
//!   themselves.
//! - **nexo PRIOR ART**: `crates/core/src/agent/workspace_git.rs`
//!   (Phase 10.9) ships `MemoryGitRepo` with secret-guard +
//!   `MAX_COMMIT_FILE_BYTES` enforcement. `src/main.rs:3640-3665`
//!   already wires the scoring-sweep dreaming to git via
//!   `commit_all`; this trait extends the symmetry to the
//!   fork-pass.

use async_trait::async_trait;

/// Sink for memory-state checkpoints. Implemented by
/// `nexo_core::agent::MemoryGitRepo` (Phase 10.9 git-backed memory);
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Trait-object-safety smoke test: must coerce to
    /// `Arc<dyn MemoryCheckpointer>` and dispatch through the vtable.
    #[tokio::test]
    async fn trait_is_object_safe_and_dispatches() {
        struct Counter(AtomicUsize);
        #[async_trait]
        impl MemoryCheckpointer for Counter {
            async fn checkpoint(
                &self,
                _subject: String,
                _body: String,
            ) -> Result<(), String> {
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
