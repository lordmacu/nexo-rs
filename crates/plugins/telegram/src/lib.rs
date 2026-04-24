//! `agent-plugin-telegram` — Telegram Bot channel plugin.
//!
//! Uses the HTTP Bot API (long polling). Complements the WhatsApp plugin
//! for multi-channel deployments: one agent, two surfaces, same session
//! semantics via `UUIDv5(chat_id)`.

pub mod bot;
pub mod events;
pub mod plugin;
pub mod session_id;
pub mod tool;

pub use events::InboundEvent;
pub use plugin::{TelegramPlugin, TOPIC_INBOUND, TOPIC_OUTBOUND};
pub use session_id::session_id_for_chat;
pub use tool::register_telegram_tools;
