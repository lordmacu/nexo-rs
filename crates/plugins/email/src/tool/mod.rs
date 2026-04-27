//! Email-channel tool handlers (Phase 48.7).

pub mod archive;
pub mod context;
pub mod imap_op;
pub mod label;
pub mod move_to;
pub mod reply;
pub mod search;
pub mod send;
pub mod uid_set;

#[cfg(test)]
pub mod dispatcher_stub;

pub use context::{DispatcherHandle, EmailToolContext};
pub use imap_op::{imap_date, imap_quote, run_imap_op};

use std::sync::Arc;

/// Register every email-channel tool against the supplied registry.
pub fn register_email_tools(
    registry: &nexo_core::agent::tool_registry::ToolRegistry,
    ctx: Arc<EmailToolContext>,
) {
    send::register(registry, ctx.clone());
    reply::register(registry, ctx.clone());
    archive::register(registry, ctx.clone());
    move_to::register(registry, ctx.clone());
    label::register(registry, ctx.clone());
    search::register(registry, ctx);
}
