//! Phase 80.1 — autoDream fork-style memory consolidation.
//!
//! Verbatim port of `claude-code-leak/src/services/autoDream/`. See
//! `README.md` for the full reference + intentional divergences.
//!
//! # Three pillars
//!
//! - **Robusto**: 23 edge cases tested; defense-in-depth (whitelist +
//!   path canonicalize + post-fork audit + lock); typed errors;
//!   idempotent rollback; symlink defense.
//! - **Óptimo**: reuses 80.18 / 80.19 / 80.20 + Phase 10.6 scoring;
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

// Modules below land in subsequent steps:
// pub mod consolidation_prompt;     // step 4
// pub mod dream_progress_watcher;   // step 5
// pub mod auto_dream;               // step 6

pub use auto_dream::{
    build_extra, AutoDreamRunner, DreamContext, RunOutcome, RunReason, SkipReason,
};
pub use boot::{build_runner, default_dream_db_path, default_memory_dir, BootDeps};
pub use tools::{register_dream_now_tool, DreamNowTool, DREAM_NOW_TOOL_NAME};
pub use config::AutoDreamConfig;
pub use consolidation_lock::{is_pid_running, list_sessions_touched_since, ConsolidationLock};
pub use consolidation_prompt::{
    ConsolidationPromptBuilder, DIR_EXISTS_GUIDANCE, ENTRYPOINT_NAME, MAX_ENTRYPOINT_LINES,
};
pub use dream_progress_watcher::{DreamProgressWatcher, ProgressResult};
pub use error::AutoDreamError;
