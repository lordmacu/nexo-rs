//! Email-channel tool handlers (Phase 48.7).

pub mod context;
pub mod imap_op;
pub mod reply;
pub mod send;

pub use context::{DispatcherHandle, EmailToolContext};
pub use imap_op::{imap_date, imap_quote, run_imap_op};

use std::sync::Arc;

/// Register every email-channel tool against the supplied registry.
/// 48.7.b registers send + reply; 48.7.c will add archive / move_to /
/// label / search.
pub fn register_email_tools(
    registry: &nexo_core::agent::tool_registry::ToolRegistry,
    ctx: Arc<EmailToolContext>,
) {
    send::register(registry, ctx.clone());
    reply::register(registry, ctx);
}
