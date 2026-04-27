//! Inbound event payload published by `AccountWorker` to
//! `plugin.inbound.email.<instance>` (Phase 48.3).
//!
//! Stays minimal on purpose — no MIME parsing, no `EmailMeta`, just
//! enough for downstream consumers to correlate the message with its
//! UID/INTERNALDATE and a raw `.eml` blob. Phase 48.5 enriches with
//! parsed headers and `Attachment` envelopes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboundEvent {
    pub account_id: String,
    pub instance: String,
    pub uid: u32,
    /// Unix seconds. IMAP `INTERNALDATE` (server-side arrival), not the
    /// `Date:` header — those can lie / drift.
    pub internal_date: i64,
    /// Raw RFC 5322 message bytes (`BODY.PEEK[]`). Binary-safe via
    /// `serde_bytes` so the broker payload doesn't pay base64 overhead.
    #[serde(with = "serde_bytes")]
    pub raw_bytes: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_serde_preserves_raw_bytes() {
        let ev = InboundEvent {
            account_id: "ops@example.com".into(),
            instance: "ops".into(),
            uid: 42,
            internal_date: 1_735_689_600,
            raw_bytes: vec![0u8, 1, 2, 250, 251, 252],
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: InboundEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }
}
