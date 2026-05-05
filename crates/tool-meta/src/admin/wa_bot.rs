//! `nexo/admin/whatsapp/bot/*` wire types.
//!
//! Phase: experimental Meta AI / `*@bot` decryption surface. Each
//! WhatsApp-bound agent owns a paired phone, and that phone has
//! its own roster of assigned AI bots (Meta AI, future personas).
//! This domain lets a microapp UI list those bots and chat with
//! them manually — separate from the agent's regular dispatcher
//! so bot replies never get re-fed as user input.
//!
//! See `wa-agent` >= 0.1.5 for the underlying capture / decrypt
//! plumbing (`MessageEvent::BotMessage`, `Session::list_bots`).

use serde::{Deserialize, Serialize};

/// JSON-RPC notification method emitted whenever a bot reply
/// chunk lands on a daemon-paired session. Payload shape:
/// [`BotMessageNotification`].
pub const BOT_MESSAGE_NOTIFY_METHOD: &str = "nexo/notify/whatsapp_bot_message";

/// Params for `nexo/admin/whatsapp/bot/list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BotListParams {
    /// Agent whose paired session should be queried. Each agent
    /// has its own phone → its own bot roster.
    pub agent_id: String,
}

/// Single bot the linked phone is allowed to chat with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BotInfo {
    /// Bot JID, e.g. `718584497008509@bot`. Send to this address
    /// via `nexo/admin/whatsapp/bot/send`.
    pub jid: String,
    /// Opaque persona id used by the bot profile follow-up query.
    pub persona_id: String,
}

/// Response for `nexo/admin/whatsapp/bot/list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BotListResponse {
    /// Echoes the `agent_id` from the request — handy when the
    /// caller fans out queries for several agents in parallel.
    pub agent_id: String,
    /// Bots assigned to this agent's paired account, in
    /// server-declared order (Meta AI is typically first).
    pub bots: Vec<BotInfo>,
}

/// Params for `nexo/admin/whatsapp/bot/send`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BotSendInput {
    /// Agent identifying the paired session.
    pub agent_id: String,
    /// Bot JID to send to (must end in `@bot`).
    pub bot_jid: String,
    /// Message body, plain text.
    pub text: String,
}

/// Response for `nexo/admin/whatsapp/bot/send`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BotSendResponse {
    /// Stanza id assigned to the outbound message — pair this with
    /// the bot's reply chunks via their `target_id` field.
    pub msg_id: String,
}

/// Push payload for [`BOT_MESSAGE_NOTIFY_METHOD`]. Mirrors
/// `wa_agent::MessageEvent::BotMessage` plus the agent id we
/// learned the message under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BotMessageNotification {
    /// Agent that owns the session this message arrived on.
    pub agent_id: String,
    /// Bot JID that sent the chunk.
    pub bot_jid: String,
    /// This chunk's stanza id (different per chunk in a streamed
    /// reply).
    pub msg_id: String,
    /// Original outbound id we sent to the bot. Constant across
    /// every chunk of one logical reply — UIs key history rows by
    /// this.
    pub target_id: String,
    /// `first` / `inner` / `last`. `last` = reply complete.
    pub edit: String,
    /// Concatenated `AIRichResponseMessage.submessages[].messageText`
    /// for this snapshot. Each chunk REPLACES the previous one —
    /// render the latest by `target_id`.
    pub text: String,
    /// Wall-clock millis stamped on receipt by the forwarder. UI
    /// uses it for bubble timestamps.
    pub at_ms: u64,
}
