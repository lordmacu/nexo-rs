//! Inbound event payload published by `AccountWorker` to
//! `plugin.inbound.email.<instance>` (Phase 48.3).
//!
//! Stays minimal on purpose — no MIME parsing, no `EmailMeta`, just
//! enough for downstream consumers to correlate the message with its
//! UID/INTERNALDATE and a raw `.eml` blob. Phase 48.5 enriches with
//! parsed headers and `Attachment` envelopes.

use std::collections::BTreeMap;

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
    /// Phase 48.5 — `Some` when MIME parse succeeded; `None` for
    /// degraded raw-only publishes (parse failure logged warn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<EmailMeta>,
    /// Phase 48.5 — empty when no attachments or `meta` is `None`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<EmailAttachment>,
}

/// Parsed metadata for an inbound message (Phase 48.5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    pub from: AddressEntry,
    #[serde(default)]
    pub to: Vec<AddressEntry>,
    #[serde(default)]
    pub cc: Vec<AddressEntry>,
    pub subject: String,
    pub body_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_html: Option<String>,
    /// Unix seconds parsed from the `Date:` header. Falls back to the
    /// `internal_date` of the surrounding `InboundEvent` when the
    /// header is missing or malformed.
    pub date: i64,
    /// Selected headers needed by 48.8 (loop prevention) and tools.
    /// Names are lowercased; consumers query by exact key.
    #[serde(default)]
    pub headers_extra: BTreeMap<String, String>,
    /// `body_text` was truncated to `max_body_bytes` (the raw
    /// `body_html` is preserved un-truncated).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub body_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AddressEntry {
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailAttachment {
    pub sha256: String,
    pub local_path: String,
    pub size_bytes: u64,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    pub disposition: AttachmentDisposition,
    /// Bytes were truncated to `max_attachment_bytes`. The on-disk
    /// file holds only the prefix; `size_bytes` reflects the truncated
    /// length, not the original.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentDisposition {
    Inline,
    #[default]
    Attachment,
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
    /// Phase 48.5 — outbound attachments by file path. Dispatcher reads
    /// the bytes at `enqueue_command` time so a missing file fails
    /// fast (ack `Failed`) instead of waiting until the next drain
    /// tick.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<OutboundAttachmentRef>,
}

/// Path-by-reference outbound attachment (Phase 48.5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboundAttachmentRef {
    /// Absolute or daemon-CWD-relative path the dispatcher reads.
    pub data_path: String,
    /// Display filename used in the `Content-Disposition` header.
    pub filename: String,
    /// `None` → inferred via `mime_guess` from `filename`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    #[serde(default)]
    pub disposition: AttachmentDisposition,
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
            meta: None,
            attachments: vec![],
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
            attachments: vec![],
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
            attachments: vec![],
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
    fn email_attachment_round_trip() {
        let a = EmailAttachment {
            sha256: "abc123".into(),
            local_path: "/data/email-attachments/abc123".into(),
            size_bytes: 4096,
            mime_type: "application/pdf".into(),
            filename: Some("report.pdf".into()),
            content_id: None,
            disposition: AttachmentDisposition::Attachment,
            truncated: false,
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: EmailAttachment = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn email_attachment_disposition_snake_case() {
        let json = r#"{"sha256":"x","local_path":"p","size_bytes":1,"mime_type":"text/plain","disposition":"inline"}"#;
        let back: EmailAttachment = serde_json::from_str(json).unwrap();
        assert!(matches!(back.disposition, AttachmentDisposition::Inline));
    }

    #[test]
    fn outbound_attachment_ref_round_trip() {
        let a = OutboundAttachmentRef {
            data_path: "/tmp/x.pdf".into(),
            filename: "x.pdf".into(),
            mime_type: Some("application/pdf".into()),
            content_id: None,
            disposition: AttachmentDisposition::Attachment,
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: OutboundAttachmentRef = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn outbound_attachment_ref_omits_optional_fields() {
        let json = r#"{"data_path":"/tmp/x","filename":"x"}"#;
        let back: OutboundAttachmentRef = serde_json::from_str(json).unwrap();
        assert!(back.mime_type.is_none());
        assert!(back.content_id.is_none());
        assert!(matches!(back.disposition, AttachmentDisposition::Attachment));
    }

    #[test]
    fn email_meta_round_trip() {
        let m = EmailMeta {
            message_id: Some("<abc@x>".into()),
            in_reply_to: None,
            references: vec![],
            from: AddressEntry {
                address: "alice@x".into(),
                name: Some("Alice".into()),
            },
            to: vec![],
            cc: vec![],
            subject: "Hi".into(),
            body_text: "hello".into(),
            body_html: None,
            date: 1_700_000_000,
            headers_extra: BTreeMap::new(),
            body_truncated: false,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: EmailMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn inbound_event_back_compat_without_meta() {
        // 48.3-shaped payload (no meta / attachments) must still parse.
        let json = r#"{"account_id":"a","instance":"i","uid":1,"internal_date":0,"raw_bytes":[]}"#;
        let back: InboundEvent = serde_json::from_str(json).unwrap();
        assert!(back.meta.is_none());
        assert!(back.attachments.is_empty());
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
