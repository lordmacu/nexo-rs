//! `nexo-plugin-whatsapp` — WhatsApp channel plugin backed by the
//! `wa-agent` crate (a.k.a. `whatsapp_rs` on the imports side).
//!
//! See `docs/wa-agent-integration.md` for the integration ADR. Phase 6.2
//! currently wires only configuration + bootstrap — inbound bridge and
//! outbound dispatcher land in 6.3 and 6.4.

pub mod bridge;
pub mod dispatch;
pub mod events;
pub mod lifecycle;
pub mod media;
pub mod pairing;
pub mod pairing_adapter;
pub mod plugin;
pub mod session;
pub mod session_id;
pub mod tool;
pub mod transcriber;

pub use pairing_adapter::WhatsappPairingAdapter;
pub use plugin::WhatsappPlugin;
pub use tool::register_whatsapp_tools;

use std::sync::Arc;

use nexo_config::WhatsappPluginConfig;
use nexo_core::agent::nexo_plugin_registry::PluginFactory;
use nexo_core::agent::plugin_host::NexoPlugin;

/// Phase 81.12.c — factory builder for one whatsapp plugin instance.
/// The closure clones `cfg` per call, so multi-account operators call
/// this once per `WhatsappPluginConfig` (one per Signal Protocol
/// session_dir / instance label) and register each result in a
/// `PluginFactoryRegistry` under a distinct manifest name.
/// `wire_plugin_registry(..., Some(&factory))` then instantiates them
/// on boot.
///
/// Today (81.12.c): exported but no caller registers it — main.rs's
/// legacy loop at `src/main.rs:1880-1897` keeps constructing
/// `WhatsappPlugin` directly. 81.12.e flips that block to use this
/// factory.
pub fn whatsapp_plugin_factory(cfg: WhatsappPluginConfig) -> PluginFactory {
    Box::new(move |_manifest| {
        let plugin: Arc<dyn NexoPlugin> = Arc::new(WhatsappPlugin::new(cfg.clone()));
        Ok(plugin)
    })
}
