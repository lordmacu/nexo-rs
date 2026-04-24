//! `agent-plugin-whatsapp` — WhatsApp channel plugin backed by the
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
pub mod plugin;
pub mod session;
pub mod session_id;
pub mod tool;
pub mod transcriber;

pub use plugin::WhatsappPlugin;
pub use tool::register_whatsapp_tools;
