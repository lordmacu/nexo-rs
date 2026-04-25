//! Phase 67.3 — MCP `permission_prompt` tool.
//!
//! See `README.md` for the layering rationale and bin modes. The
//! crate exposes the contract (types + trait + handler); the daemon
//! wiring lands in 67.4.

pub mod adapter;
pub mod cache;
pub mod decider;
pub mod error;
pub mod mcp;
pub mod types;

pub use adapter::outcome_to_claude_value;
pub use cache::{fingerprint, SessionCacheKey};
pub use decider::{AllowAllDecider, DenyAllDecider, PermissionDecider, ScriptedDecider};
pub use error::PermissionError;
pub use mcp::PermissionMcpServer;
pub use types::{AllowScope, PermissionOutcome, PermissionRequest, PermissionResponse};
