//! Phase 84 — role-aware persona system prompts.
//!
//! Phase 77.18 introduced `BindingRole::{Coordinator, Worker, …}` and
//! restricted the team-coordination tool surface to coordinator
//! bindings. The role flag was purely a tool-gate signal — the
//! coordinator agent itself ran the standard system prompt and had
//! no awareness that it was orchestrating workers.
//!
//! This module closes that gap. Each persona builder returns a single
//! prompt block (Markdown-formatted) that the boot path prepends to
//! the agent's existing `system_prompt` when the binding's resolved
//! role matches. Bindings with `BindingRole::Unset` see no persona
//! block, preserving today's behaviour byte-for-byte.

pub mod coordinator;

pub use coordinator::{coordinator_system_prompt, CoordinatorPromptCtx};
