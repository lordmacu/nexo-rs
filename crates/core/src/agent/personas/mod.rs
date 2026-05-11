//! Role-aware persona system prompts.
//!
//! `BindingRole::{Coordinator, Worker, …}` originally only restricted
//! the team-coordination tool surface to coordinator bindings — a
//! pure tool-gate signal, with no effect on the system prompt itself.
//!
//! This module closes that gap. Each persona builder returns a single
//! prompt block (Markdown-formatted) that the boot path prepends to
//! the agent's existing `system_prompt` when the binding's resolved
//! role matches. Bindings with `BindingRole::Unset` see no persona
//! block, preserving today's behaviour byte-for-byte.

pub mod coordinator;
pub mod worker;

pub use coordinator::{coordinator_system_prompt, CoordinatorPromptCtx};
pub use worker::{worker_system_prompt, WorkerPromptCtx};
