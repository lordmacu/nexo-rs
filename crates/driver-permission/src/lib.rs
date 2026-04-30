//! Phase 67.3 — MCP `permission_prompt` tool.
//!
//! See `README.md` for the layering rationale and bin modes. The
//! crate exposes the contract (types + trait + handler); the daemon
//! wiring lands in 67.4.

pub mod adapter;
pub mod advisor;
pub mod auto_approve;
pub mod bash_destructive;
pub mod cache;

pub use auto_approve::{
    is_curated_auto_approve, AutoApproveDecider, META_AUTO_APPROVE, META_WORKSPACE_PATH,
};
pub mod decider;
pub mod error;
pub mod mcp;
pub mod path_extractor;
pub mod sed_validator;
pub mod should_use_sandbox;
#[cfg(unix)]
pub mod socket;
pub mod types;

pub use adapter::outcome_to_claude_value;
pub use advisor::{AdvisorRegistry, BashSecurityAdvisor, ToolAdvisor};
pub use cache::{fingerprint, SessionCacheKey};
pub use decider::{AllowAllDecider, DenyAllDecider, PermissionDecider, ScriptedDecider};
pub use error::PermissionError;
pub use mcp::PermissionMcpServer;
#[cfg(unix)]
pub use socket::SocketDecider;
pub use types::{AllowScope, PermissionOutcome, PermissionRequest, PermissionResponse};
