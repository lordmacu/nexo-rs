//! Built-in pollers shipped with the framework. To add a new one:
//!
//! 1. Create `crates/poller/src/builtins/<your_kind>.rs` with a struct
//!    that `impl Poller`.
//! 2. Add a `pub mod` line below.
//! 3. Push your struct into [`register_all`].
//!
//! That is the only place wiring is touched — `main.rs` calls
//! `register_all` once at boot. See `docs/src/recipes/build-a-poller.md`
//! for the full pattern.

use std::sync::Arc;

use crate::PollerRunner;

pub mod agent_turn;
pub mod gmail;
pub mod google_calendar;
pub mod rss;
pub mod webhook_poll;

/// Register every built-in poller. Single touch point — adding a new
/// module means dropping a `pub mod x;` above and one `runner.register`
/// line below. Idempotent: re-registering replaces the previous impl.
pub fn register_all(runner: &PollerRunner) {
    runner.register(Arc::new(gmail::GmailPoller::new()));
    runner.register(Arc::new(rss::RssPoller::new()));
    runner.register(Arc::new(webhook_poll::WebhookPoller::new()));
    runner.register(Arc::new(google_calendar::GoogleCalendarPoller::new()));
    runner.register(Arc::new(agent_turn::AgentTurnPoller::new()));
}
