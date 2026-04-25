//! Goal workspace management. Phase 67.6 added git-worktree mode
//! on top of the 67.4 mkdir + traversal-guard surface.

pub mod git;
pub mod manager;

pub use manager::{GitWorktreeMode, WorkspaceManager};
