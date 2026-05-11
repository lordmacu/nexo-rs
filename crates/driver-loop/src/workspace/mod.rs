//! Goal workspace management. Supports a git-worktree mode on top of
//! the plain mkdir + traversal-guard surface.

pub mod git;
pub mod manager;

pub use manager::{GitWorktreeMode, WorkspaceManager};
