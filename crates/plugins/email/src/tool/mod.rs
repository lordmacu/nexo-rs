//! Email-channel tool handlers (Phase 48.7).

pub mod context;
pub mod imap_op;

pub use context::{DispatcherHandle, EmailToolContext};
pub use imap_op::{imap_date, imap_quote, run_imap_op};

use std::sync::Arc;

/// Register every email-channel tool against the supplied registry.
/// 48.7.a is the foundational slice; the individual `ToolHandler`
/// impls land in 48.7.b/c.
pub fn register_email_tools(
    _registry: &nexo_core::agent::tool_registry::ToolRegistry,
    _ctx: Arc<EmailToolContext>,
) {
    // 48.7.b — send::register, reply::register
    // 48.7.c — archive::register, move_to::register, label::register, search::register
}
