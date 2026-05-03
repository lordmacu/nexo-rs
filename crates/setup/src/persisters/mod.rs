//! Phase 82.10.n — production [`ChannelCredentialPersister`]
//! impls bridging the admin RPC `credentials/register` flow to
//! per-channel plugin yaml + secret files.
//!
//! Persisters live in `nexo-setup` (not in the per-plugin
//! crates) because:
//! 1. `nexo-setup` already depends on the plugin crates for
//!    other adapters (`admin_adapters.rs`); the inverse direction
//!    would create circular deps.
//! 2. The yaml-patch helpers (`crate::yaml_patch::*`) live here
//!    too, so co-locating the persister keeps the bridge layer
//!    in one crate.
//!
//! Boot wires each persister via
//! [`crate::admin_bootstrap::AdminBootstrap`]; the dispatcher
//! routes `credentials/register` per-channel based on the
//! `input.channel` field.
//!
//! [`ChannelCredentialPersister`]: nexo_core::agent::admin_rpc::domains::credentials::ChannelCredentialPersister

pub mod email;
pub mod telegram;
pub mod whatsapp;

pub use email::EmailPersister;
pub use telegram::TelegramPersister;
pub use whatsapp::WhatsappPersister;
