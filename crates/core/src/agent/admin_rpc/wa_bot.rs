//! `nexo/admin/whatsapp/bot/*` handle.
//!
//! The dispatcher holds an `Arc<dyn WaBotHandle>` populated by the
//! WhatsApp channel plugin at boot. Two methods only:
//!
//!   * `list_bots(agent_id)` — calls
//!     `wa_agent::Session::list_bots()` against the paired session
//!     for that agent.
//!   * `send_to_bot(agent_id, jid, text)` — calls
//!     `Session::send_text(jid, text)`. The bot reply path
//!     (`MessageEvent::BotMessage`) is captured by a separate
//!     subscriber that fans the chunks out as
//!     `nexo/notify/whatsapp_bot_message` notifications.
//!
//! Both methods return `Err` when the agent is unknown or the
//! WhatsApp session isn't connected yet — callers surface that to
//! the operator UI.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_tool_meta::admin::wa_bot::BotInfo;

/// Result of a bot-channel call. Plain `anyhow::Result` so plugin
/// implementations don't have to design a bespoke error type.
pub type WaBotResult<T> = anyhow::Result<T>;

/// Per-channel hook into a paired WhatsApp session. Implemented by
/// `nexo-plugin-whatsapp` and registered with the dispatcher at
/// daemon boot.
#[async_trait]
pub trait WaBotHandle: Send + Sync + 'static {
    /// List the AI bots WhatsApp has assigned to the account
    /// paired to `agent_id`. Returns an empty `Vec` when the
    /// account has no bots, `Err` when the agent is unknown or
    /// the session isn't connected.
    async fn list_bots(&self, agent_id: &str) -> WaBotResult<Vec<BotInfo>>;

    /// Send a plain-text message from `agent_id`'s paired session
    /// to `bot_jid`. Returns the outbound stanza id.
    async fn send_to_bot(&self, agent_id: &str, bot_jid: &str, text: &str) -> WaBotResult<String>;
}

/// Type-alias for the dispatcher field. `None` when the daemon is
/// running without a WhatsApp plugin (e.g. test harnesses,
/// non-channel demos).
pub type WaBotHandleArc = Option<Arc<dyn WaBotHandle>>;
