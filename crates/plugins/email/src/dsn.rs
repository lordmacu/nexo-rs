//! Delivery-status-notification (DSN) parsing (Phase 48.8).
//!
//! Inbound delivery reports never reach the LLM as conversational
//! content — they're operational signals. `parse_bounce` lifts the
//! recognisable fields (`Action`, `Status`, `Final-Recipient`,
//! `Diagnostic-Code`) out of a `multipart/report; report-type=
//! delivery-status` payload, falling back to a heuristic
//! MAILER-DAEMON localpart match when the report-type marker is
//! missing (some legacy MTAs do this).
//!
//! `BounceClassification` follows SMTP convention: `5.x.x` →
//! Permanent, `4.x.x` → Transient, anything else → Unknown.

use mail_parser::{MessageParser, MimeHeaders};
use serde::{Deserialize, Serialize};

use crate::events::EmailMeta;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BounceClassification {
    Permanent,
    Transient,
    #[default]
    Unknown,
}

impl BounceClassification {
    pub fn from_status_code(code: Option<&str>) -> Self {
        match code.and_then(|s| s.trim().chars().next()) {
            Some('5') => Self::Permanent,
            Some('4') => Self::Transient,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedBounce {
    pub original_message_id: Option<String>,
    pub recipient: Option<String>,
    pub status_code: Option<String>,
    pub action: Option<String>,
    pub reason: Option<String>,
    pub classification: BounceClassification,
}

/// Wire payload published on `email.bounce.<instance>`. The worker
/// constructs this from a `ParsedBounce` plus the surrounding
/// account context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BounceEvent {
    pub account_id: String,
    pub instance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub classification: BounceClassification,
}

const MAILER_DAEMON_LOCALPARTS: &[&str] = &[
    "mailer-daemon",
    "mail-daemon",
    "mail.daemon",
    "postmaster",
];

/// Try to read `raw_bytes` as a delivery report. Returns `None`
/// when the message is regular conversational mail. Caller (the
/// inbound worker) supplies the surrounding `account_id` /
/// `instance` to assemble the wire `BounceEvent`.
pub fn parse_bounce(
    meta: &EmailMeta,
    raw_bytes: &[u8],
    account_address: &str,
) -> Option<ParsedBounce> {
    let parsed = MessageParser::default().parse(raw_bytes)?;

    // Primary path: DSN content-type marker.
    let mut dsn = false;
    if let Some(ct) = parsed.content_type() {
        if ct.ctype().eq_ignore_ascii_case("multipart")
            && ct
                .subtype()
                .map(|s| s.eq_ignore_ascii_case("report"))
                .unwrap_or(false)
        {
            // `report-type=delivery-status` parameter.
            if ct
                .attribute("report-type")
                .map(|v| v.eq_ignore_ascii_case("delivery-status"))
                .unwrap_or(false)
            {
                dsn = true;
            }
        }
    }

    // Heuristic fallback: MAILER-DAEMON / postmaster sender even
    // when the message lacks `multipart/report`. Some legacy MTAs
    // ship plain text/plain bounces.
    //
    // Audit follow-up D — require the sender's *domain* to match
    // the recipient account's domain (or a sub/parent of it).
    // Otherwise a spoofed `From: mailer-daemon@attacker.com` could
    // poison the bounce store. Real DSNs come from an MTA in the
    // same hosting domain as the mailbox; cross-domain MAILER-
    // DAEMONs are noise we don't want to count.
    let from_local = meta
        .from
        .address
        .split_once('@')
        .map(|(l, _)| l.trim().to_ascii_lowercase());
    let from_domain = meta
        .from
        .address
        .rsplit_once('@')
        .map(|(_, d)| d.trim().to_ascii_lowercase());
    let account_domain = account_address
        .rsplit_once('@')
        .map(|(_, d)| d.trim().to_ascii_lowercase());
    let domain_aligned = match (from_domain.as_deref(), account_domain.as_deref()) {
        (Some(f), Some(a)) => {
            // Exact, sub-domain (mta.example.com vs example.com),
            // or super-domain (example.com vs mta.example.com)
            // all count. Empty domain on either side fails.
            !a.is_empty()
                && !f.is_empty()
                && (f == a
                    || f.ends_with(&format!(".{a}"))
                    || a.ends_with(&format!(".{f}")))
        }
        _ => false,
    };
    let heuristic = from_local
        .as_deref()
        .map(|l| MAILER_DAEMON_LOCALPARTS.iter().any(|m| l.eq_ignore_ascii_case(m)))
        .unwrap_or(false)
        && domain_aligned;

    if !dsn && !heuristic {
        return None;
    }

    // Walk parts looking for `message/delivery-status` (the
    // structured report) and `message/rfc822` /
    // `text/rfc822-headers` (the original message snapshot).
    let mut bounce = ParsedBounce::default();

    for part in parsed.parts.iter() {
        let Some(ct) = part.content_type() else {
            continue;
        };
        let ctype = ct.ctype().to_ascii_lowercase();
        let subtype = ct
            .subtype()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();

        if ctype == "message" && subtype == "delivery-status" {
            // Body is a tiny RFC 822-like document with key:value
            // lines per recipient. Parse manually — mail-parser
            // doesn't expose the inner headers as a Message.
            let body = part.contents();
            let s = String::from_utf8_lossy(body);
            extract_delivery_status_fields(&s, &mut bounce);
        } else if ctype == "message" && subtype == "rfc822" {
            // Original message snapshot. Re-parse to lift its
            // Message-ID.
            let inner_bytes = part.contents();
            if let Some(inner) = MessageParser::default().parse(inner_bytes) {
                if let Some(mid) = inner.message_id() {
                    bounce.original_message_id = Some(format!("<{}>", mid));
                }
            }
        } else if ctype == "text" && subtype == "rfc822-headers" {
            // Cheaper variant: just the headers of the original.
            let body = part.contents();
            let s = String::from_utf8_lossy(body);
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("Message-ID:") {
                    let v = rest.trim();
                    if !v.is_empty() {
                        bounce.original_message_id = Some(v.to_string());
                        break;
                    }
                }
                if let Some(rest) = line.strip_prefix("Message-Id:") {
                    let v = rest.trim();
                    if !v.is_empty() {
                        bounce.original_message_id = Some(v.to_string());
                        break;
                    }
                }
            }
        }
    }

    // Pull `Diagnostic-Code` reason as fallback when `Status:`
    // alone wasn't enough. Already done inside the helper, but if
    // the report was missing we still want classification Unknown
    // so the agent / metric sees something.
    bounce.classification = BounceClassification::from_status_code(bounce.status_code.as_deref());

    // Avoid silently swallowing the bounce: if heuristic-only and
    // no fields found, we still return the bounce shape with
    // classification Unknown so downstream sees the signal.
    Some(bounce)
}

/// Extract the `Action` / `Status` / `Final-Recipient` /
/// `Diagnostic-Code` lines from a `message/delivery-status` body.
/// Tolerates folded lines and missing fields.
fn extract_delivery_status_fields(body: &str, out: &mut ParsedBounce) {
    let mut current: Option<&'static str> = None;
    let mut buf = String::new();
    for raw_line in body.lines() {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            buf.push(' ');
            buf.push_str(raw_line.trim());
            continue;
        }
        // Flush the previous header into the slot before
        // starting a new one.
        flush_dsn_field(current, &buf, out);
        buf.clear();
        current = None;

        if let Some((name, value)) = raw_line.split_once(':') {
            let n = name.trim().to_ascii_lowercase();
            let key = match n.as_str() {
                "action" => Some("action"),
                "status" => Some("status"),
                "final-recipient" => Some("final_recipient"),
                "original-recipient" => {
                    // Fall back to original-recipient only when
                    // we haven't already seen final-recipient.
                    if out.recipient.is_none() {
                        Some("final_recipient")
                    } else {
                        None
                    }
                }
                "diagnostic-code" => Some("diagnostic_code"),
                _ => None,
            };
            current = key;
            buf = value.trim().to_string();
        }
    }
    flush_dsn_field(current, &buf, out);
}

fn flush_dsn_field(current: Option<&'static str>, buf: &str, out: &mut ParsedBounce) {
    let Some(key) = current else { return };
    let v = buf.trim();
    if v.is_empty() {
        return;
    }
    match key {
        "action" => {
            if out.action.is_none() {
                out.action = Some(v.to_string());
            }
        }
        "status" => {
            if out.status_code.is_none() {
                out.status_code = Some(v.to_string());
            }
        }
        "final_recipient" => {
            if out.recipient.is_none() {
                // `Final-Recipient: rfc822; ghost@x` → strip
                // the `addr-type;` prefix.
                let addr = match v.split_once(';') {
                    Some((_, rest)) => rest.trim().to_string(),
                    None => v.to_string(),
                };
                out.recipient = Some(addr);
            }
        }
        "diagnostic_code" => {
            if out.reason.is_none() {
                out.reason = Some(v.to_string());
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AddressEntry;
    use std::collections::BTreeMap;

    fn meta(from: &str) -> EmailMeta {
        EmailMeta {
            message_id: None,
            in_reply_to: None,
            references: vec![],
            from: AddressEntry {
                address: from.into(),
                name: None,
            },
            to: vec![],
            cc: vec![],
            subject: String::new(),
            body_text: String::new(),
            body_html: None,
            date: 0,
            headers_extra: BTreeMap::new(),
            body_truncated: false,
        }
    }

    #[test]
    fn classification_5xx_is_permanent() {
        assert_eq!(
            BounceClassification::from_status_code(Some("5.1.1")),
            BounceClassification::Permanent
        );
    }

    #[test]
    fn classification_4xx_is_transient() {
        assert_eq!(
            BounceClassification::from_status_code(Some("4.7.0")),
            BounceClassification::Transient
        );
    }

    #[test]
    fn classification_missing_is_unknown() {
        assert_eq!(
            BounceClassification::from_status_code(None),
            BounceClassification::Unknown
        );
    }

    const POSTFIX_DSN: &[u8] = b"From: MAILER-DAEMON@mail.example.com\r\n\
To: ops@example.com\r\n\
Subject: Undelivered Mail Returned to Sender\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/report; report-type=delivery-status; boundary=\"BOUND\"\r\n\
\r\n\
--BOUND\r\n\
Content-Type: text/plain; charset=us-ascii\r\n\
\r\n\
This is the mail system at host mail.example.com.\r\n\
\r\n\
--BOUND\r\n\
Content-Type: message/delivery-status\r\n\
\r\n\
Reporting-MTA: dns; mail.example.com\r\n\
\r\n\
Final-Recipient: rfc822; ghost@unknown.com\r\n\
Action: failed\r\n\
Status: 5.1.1\r\n\
Diagnostic-Code: smtp; 550 5.1.1 user unknown\r\n\
\r\n\
--BOUND\r\n\
Content-Type: text/rfc822-headers\r\n\
\r\n\
Message-ID: <orig@example.com>\r\n\
From: ops@example.com\r\n\
To: ghost@unknown.com\r\n\
\r\n\
--BOUND--\r\n";

    #[test]
    fn parses_postfix_dsn() {
        let m = meta("MAILER-DAEMON@mail.example.com");
        let b = parse_bounce(&m, POSTFIX_DSN, "ops@example.com").expect("dsn parsed");
        assert_eq!(b.recipient.as_deref(), Some("ghost@unknown.com"));
        assert_eq!(b.action.as_deref(), Some("failed"));
        assert_eq!(b.status_code.as_deref(), Some("5.1.1"));
        assert!(b.reason.as_deref().unwrap().contains("user unknown"));
        assert_eq!(b.classification, BounceClassification::Permanent);
        assert_eq!(b.original_message_id.as_deref(), Some("<orig@example.com>"));
    }

    const TRANSIENT_DSN: &[u8] = b"From: postmaster@example.com\r\n\
To: ops@example.com\r\n\
Subject: Delivery delayed\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/report; report-type=delivery-status; boundary=\"X\"\r\n\
\r\n\
--X\r\n\
Content-Type: text/plain\r\n\
\r\n\
will retry\r\n\
\r\n\
--X\r\n\
Content-Type: message/delivery-status\r\n\
\r\n\
Final-Recipient: rfc822; alice@x.com\r\n\
Action: delayed\r\n\
Status: 4.7.0\r\n\
\r\n\
--X--\r\n";

    #[test]
    fn parses_transient_dsn() {
        let m = meta("postmaster@example.com");
        let b = parse_bounce(&m, TRANSIENT_DSN, "ops@example.com").expect("dsn parsed");
        assert_eq!(b.classification, BounceClassification::Transient);
        assert_eq!(b.action.as_deref(), Some("delayed"));
        assert_eq!(b.recipient.as_deref(), Some("alice@x.com"));
    }

    const HEURISTIC_PLAIN: &[u8] = b"From: mailer-daemon@x.com\r\n\
To: ops@example.com\r\n\
Subject: Mail Delivery Failure\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
The recipient could not be reached.\r\n";

    #[test]
    fn heuristic_mailer_daemon_without_dsn_marker() {
        let m = meta("mailer-daemon@x.com");
        let b = parse_bounce(&m, HEURISTIC_PLAIN, "ops@x.com").expect("heuristic match");
        assert!(b.recipient.is_none());
        assert!(b.status_code.is_none());
        assert_eq!(b.classification, BounceClassification::Unknown);
    }

    #[test]
    fn regular_mail_returns_none() {
        let m = meta("alice@x.com");
        let plain = b"From: alice@x.com\r\nTo: ops@example.com\r\nSubject: hi\r\nMIME-Version: 1.0\r\nContent-Type: text/plain\r\n\r\nhi\r\n";
        assert!(parse_bounce(&m, plain, "ops@x.com").is_none());
    }

    #[test]
    fn malformed_dsn_returns_partial() {
        let m = meta("MAILER-DAEMON@x");
        let raw = b"From: MAILER-DAEMON@x\r\n\
To: ops@x\r\n\
Subject: bounce\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/report; report-type=delivery-status; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/plain\r\n\
\r\n\
oops\r\n\
\r\n\
--B--\r\n";
        let b = parse_bounce(&m, raw, "ops@x").expect("still recognised as bounce");
        // No delivery-status part → Unknown classification, fields empty.
        assert!(b.recipient.is_none());
        assert!(b.status_code.is_none());
        assert_eq!(b.classification, BounceClassification::Unknown);
    }

    #[test]
    fn final_recipient_strips_addr_type_prefix() {
        let mut out = ParsedBounce::default();
        let body = "Final-Recipient: rfc822; ghost@x.com\nAction: failed\nStatus: 5.1.1\n";
        extract_delivery_status_fields(body, &mut out);
        assert_eq!(out.recipient.as_deref(), Some("ghost@x.com"));
    }
}
