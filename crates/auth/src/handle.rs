use std::fmt;
use std::sync::Arc;

use sha2::{Digest, Sha256};

pub type AgentId = Arc<str>;
pub type Channel = &'static str;

pub const WHATSAPP: Channel = "whatsapp";
pub const TELEGRAM: Channel = "telegram";
pub const GOOGLE: Channel = "google";

/// 8-byte prefix of `sha256(account_id)`. Stable across boots and
/// safe to emit to logs, metrics, and transcripts — unlike the raw
/// account id it never discloses a phone number or email.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 8]);

impl Fingerprint {
    pub fn of(value: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(value.as_bytes());
        let digest = hasher.finalize();
        let mut out = [0u8; 8];
        out.copy_from_slice(&digest[..8]);
        Self(out)
    }

    pub fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Opaque handle to an issued credential. The raw `account_id` is
/// intentionally not exposed via `Debug` / `Display` — callers that
/// need to log must use [`CredentialHandle::fingerprint`]. This keeps
/// phone numbers and emails out of transcripts, audit logs, and the
/// prompt cache.
#[derive(Clone)]
pub struct CredentialHandle {
    channel: Channel,
    account_id: Arc<str>,
    agent_id: AgentId,
    fingerprint: Fingerprint,
}

impl CredentialHandle {
    pub fn new(channel: Channel, account_id: &str, agent_id: &str) -> Self {
        Self {
            channel,
            account_id: Arc::from(account_id),
            agent_id: Arc::from(agent_id),
            fingerprint: Fingerprint::of(account_id),
        }
    }

    pub fn channel(&self) -> Channel {
        self.channel
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// Raw account id — only call from the owning store when issuing
    /// a network request. Never log this value; prefer [`Self::fingerprint`].
    pub fn account_id_raw(&self) -> &str {
        &self.account_id
    }
}

impl fmt::Debug for CredentialHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialHandle")
            .field("channel", &self.channel)
            .field("agent", &self.agent_id)
            .field("fp", &self.fingerprint)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable() {
        let a = Fingerprint::of("ana@gmail.com");
        let b = Fingerprint::of("ana@gmail.com");
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_differs_between_ids() {
        let a = Fingerprint::of("ana@gmail.com");
        let b = Fingerprint::of("kate@gmail.com");
        assert_ne!(a, b);
    }

    #[test]
    fn handle_debug_does_not_leak_account_id() {
        let h = CredentialHandle::new(WHATSAPP, "+573001234567", "ana");
        let rendered = format!("{:?}", h);
        assert!(!rendered.contains("573001234567"));
        assert!(rendered.contains("whatsapp"));
        assert!(rendered.contains("ana"));
        assert!(rendered.contains(&h.fingerprint().to_hex()));
    }

    #[test]
    fn fingerprint_display_is_hex() {
        let fp = Fingerprint::of("x");
        assert_eq!(fp.to_hex().len(), 16);
        assert!(fp.to_hex().chars().all(|c| c.is_ascii_hexdigit()));
    }
}
