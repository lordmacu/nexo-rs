//! AutoDream fork-style memory consolidation.
//!
//! See `README.md` for the full reference + intentional divergences.
//!
//! # Three pillars
//!
//! - **Robusto**: 23 edge cases tested; defense-in-depth (whitelist +
//!   path canonicalize + post-fork audit + lock); typed errors;
//!   idempotent rollback; symlink defense.
//! - **Óptimo**: reuses the dream-run store + memory scoring;
//!   single canonicalize at construction; lock mtime IS
//!   lastConsolidatedAt (one stat per turn).
//! - **Transversal**: provider-agnostic via `nexo_fork::DefaultForkSubagent`;
//!   tested under 5 mock provider shapes.

pub mod auto_dream;
pub mod boot;
pub mod config;
pub mod consolidation_lock;
pub mod consolidation_prompt;
pub mod dream_progress_watcher;
pub mod error;
pub mod tools;

pub use auto_dream::{
    build_extra, AutoDreamRunner, DreamContext, RunOutcome, RunReason, SkipReason,
};
pub use boot::{build_runner, default_dream_db_path, default_memory_dir, BootDeps};
pub use config::AutoDreamConfig;
pub use consolidation_lock::{is_pid_running, list_sessions_touched_since, ConsolidationLock};
pub use consolidation_prompt::{
    ConsolidationPromptBuilder, DIR_EXISTS_GUIDANCE, ENTRYPOINT_NAME, MAX_ENTRYPOINT_LINES,
};
pub use dream_progress_watcher::{DreamProgressWatcher, ProgressResult};
pub use error::AutoDreamError;
pub use tools::{register_dream_now_tool, DreamNowTool, DREAM_NOW_TOOL_NAME};
