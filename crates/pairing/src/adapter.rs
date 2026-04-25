//! Per-channel adapter the plugins implement.
//!
//! Lets `crates/pairing/` stay channel-agnostic: the gate decides what
//! to do; the adapter knows how to actually talk to whatsapp /
//! telegram / etc.

use async_trait::async_trait;

#[async_trait]
pub trait PairingChannelAdapter: Send + Sync {
    /// Stable string id matching what the gate stores in
    /// `pairing_pending.channel` and `pairing_allow_from.channel`.
    fn channel_id(&self) -> &'static str;

    /// Normalise an inbound sender id to the canonical form the store
    /// uses. Return `None` to reject (gate treats as Drop).
    /// Examples:
    /// - whatsapp: `573...@c.us` → `+573...` (E.164)
    /// - telegram: `@User_Name` → `@user_name`; numeric chat_id passes through
    fn normalize_sender(&self, raw: &str) -> Option<String>;

    /// Send a plain-text reply. Used by the challenge flow to deliver
    /// the pairing code.
    async fn send_reply(
        &self,
        account: &str,
        to: &str,
        text: &str,
    ) -> anyhow::Result<()>;

    /// Send a QR PNG (used by `agent pair start --send-via <channel>`).
    /// Implementations that don't support media can return `Err`.
    async fn send_qr_image(
        &self,
        account: &str,
        to: &str,
        png: &[u8],
    ) -> anyhow::Result<()>;
}
