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

/// Outbound request the dispatcher consumes from
/// `plugin.outbound.email.<instance>` (Phase 48.4).
///
/// `from` is intentionally absent — it's pinned to
/// `EmailAccountConfig.address` to prevent agents from spoofing the
/// sending identity. `in_reply_to` and `references` are passthrough in
/// 48.4 (caller may not set them); 48.6 enriches them by walking the
/// thread state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboundCommand {
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AckStatus {
    /// SMTP server accepted the message (250 / 251).
    Sent,
    /// Permanent failure (5xx) or attempts >= max — moved to DLQ.
    Failed,
    /// Transient failure (4xx) or CB open — back on the queue with a
    /// later `next_attempt_at`.
    Retrying,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboundAck {
    pub message_id: String,
    pub status: AckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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

    #[test]
    fn outbound_command_roundtrip() {
        let cmd = OutboundCommand {
            to: vec!["alice@x.com".into()],
            cc: vec![],
            bcc: vec!["audit@x.com".into()],
            subject: "Hi".into(),
            body: "hello".into(),
            in_reply_to: None,
            references: vec![],
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: OutboundCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn outbound_command_threading_passthrough() {
        let cmd = OutboundCommand {
            to: vec!["alice@x.com".into()],
            cc: vec![],
            bcc: vec![],
            subject: "Re: Hi".into(),
            body: "yes".into(),
            in_reply_to: Some("<root@x.com>".into()),
            references: vec!["<root@x.com>".into(), "<reply1@x.com>".into()],
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: OutboundCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(back.in_reply_to.as_deref(), Some("<root@x.com>"));
        assert_eq!(back.references.len(), 2);
    }

    #[test]
    fn outbound_ack_sent_omits_reason() {
        let ack = OutboundAck {
            message_id: "<abc@x>".into(),
            status: AckStatus::Sent,
            reason: None,
        };
        let json = serde_json::to_string(&ack).unwrap();
        assert!(!json.contains("reason"));
        let back: OutboundAck = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, AckStatus::Sent);
    }

    #[test]
    fn outbound_ack_failed_carries_reason() {
        let ack = OutboundAck {
            message_id: "<abc@x>".into(),
            status: AckStatus::Failed,
            reason: Some("550 No such user".into()),
        };
        let json = serde_json::to_string(&ack).unwrap();
        let back: OutboundAck = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, AckStatus::Failed);
        assert_eq!(back.reason.as_deref(), Some("550 No such user"));
    }
}
