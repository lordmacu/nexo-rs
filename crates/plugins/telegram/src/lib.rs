//! `nexo-plugin-telegram` — Telegram Bot channel plugin.
//!
//! Uses the HTTP Bot API (long polling). Complements the WhatsApp plugin
//! for multi-channel deployments: one agent, two surfaces, same session
//! semantics via `UUIDv5(chat_id)`.

pub mod bot;
pub mod events;
pub mod pairing_adapter;
pub mod plugin;
pub mod session_id;
pub mod tool;

pub use events::InboundEvent;
pub use pairing_adapter::TelegramPairingAdapter;
pub use plugin::{TelegramPlugin, TOPIC_INBOUND, TOPIC_OUTBOUND};
pub use session_id::session_id_for_chat;
pub use tool::register_telegram_tools;

use std::sync::Arc;

use nexo_config::types::plugins::TelegramPluginConfig;
use nexo_core::agent::nexo_plugin_registry::PluginFactory;
use nexo_core::agent::plugin_host::NexoPlugin;

/// Phase 81.12.b — factory builder for one telegram plugin instance.
/// The closure clones `cfg` per call, so multi-bot operators call this
/// once per `TelegramPluginConfig` (one per bot token / instance label)
/// and register each result in a `PluginFactoryRegistry` under a
/// distinct manifest name. `wire_plugin_registry(..., Some(&factory))`
/// then instantiates them on boot.
///
/// Today (81.12.b): exported but no caller registers it — main.rs's
/// legacy loop at `src/main.rs:1902-1910` keeps constructing
/// `TelegramPlugin` directly. 81.12.e flips that block to use this
/// factory.
pub fn telegram_plugin_factory(cfg: TelegramPluginConfig) -> PluginFactory {
    Box::new(move |_manifest| {
        let plugin: Arc<dyn NexoPlugin> = Arc::new(TelegramPlugin::new(cfg.clone()));
        Ok(plugin)
    })
}
