//! In-tree built-in pollers shipped with the framework.
//!
//! After Phase 96 only two builtins remain in-tree:
//!  - `webhook_poll` — generic HTTP poller, not provider-specific.
//!  - `agent_turn`   — generic scheduled LLM prompt, not
//!                     provider-specific (LLM access via
//!                     `PollerHost::llm_invoke`).
//!
//! Provider-specific pollers (`gmail`, `google_calendar`, `rss`) ship
//! as standalone subprocess plugins via the `[plugin.poller]`
//! manifest section + `nexo-microapp-sdk::poller` adapter. The legacy
//! `nexo-poller-ext` path is deprecated.

use std::sync::Arc;

use crate::PollerRunner;

pub mod agent_turn;
pub mod webhook_poll;

/// Register the two in-tree built-in pollers. Single touch point —
/// adding a new in-tree builtin means dropping a `pub mod x;` above
/// and one `runner.register` line below. Idempotent: re-registering
/// the same kind replaces the previous impl.
///
/// Out-of-tree (plugin v2) pollers are discovered separately by the
/// daemon from each plugin manifest's `[plugin.poller]` section.
pub fn register_all(runner: &PollerRunner) {
    runner.register(Arc::new(webhook_poll::WebhookPoller::new()));
    runner.register(Arc::new(agent_turn::AgentTurnPoller::new()));
}
