//! Text-only MIME builder + Message-ID generator (Phase 48.4).
//!
//! Scope is intentionally tiny: we ship plain `text/plain; charset=utf-8`
//! email so the SMTP path can land before MIME plumbing (multipart,
//! attachments, HTML) arrives in Phase 48.5. Callers that need
//! attachments wait for that phase.
//!
//! `generate_message_id` is the load-bearing dedupe primitive: a
//! pre-generated `Message-ID` lets us safely reissue an SMTP `DATA`
//! after a crash, since RFC-conformant servers (Gmail, O365, Outlook)
//! drop duplicates by header. Postfix-vanilla doesn't, so the queue
//! still risks a dup on those — documented operator tradeoff.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::events::OutboundCommand;

/// Build `<{ts_ms}.{uuid_v4}@{domain}>`. `domain` is the part after
/// `@` in `from`, falling back to `nexo` when `from` is malformed —
/// some servers reject Message-IDs whose host doesn't look like a
/// hostname, so we prefer the sender's domain over a random one.
pub fn generate_message_id(from: &str) -> String {
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or("nexo");
    let domain = if domain.is_empty() { "nexo" } else { domain };
    let ts_ms = Utc::now().timestamp_millis();
    let uuid = Uuid::new_v4().simple();
    format!("<{ts_ms}.{uuid}@{domain}>")
}

/// Render an RFC 5322 text/plain message. The body is emitted as
/// `Content-Transfer-Encoding: 8bit`; mail-builder-grade quoted-
/// printable encoding lands with multipart in 48.5.
pub fn build_text_mime(
    message_id: &str,
    from: &str,
    cmd: &OutboundCommand,
    now: DateTime<Utc>,
) -> Vec<u8> {
    let mut out = String::with_capacity(256 + cmd.body.len());

    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    out.push_str("Content-Transfer-Encoding: 8bit\r\n");
    out.push_str(&format!("Date: {}\r\n", rfc2822(now)));
    out.push_str(&format!("Message-ID: {message_id}\r\n"));
    out.push_str(&format!("From: {from}\r\n"));
    if !cmd.to.is_empty() {
        out.push_str(&format!("To: {}\r\n", cmd.to.join(", ")));
    }
    if !cmd.cc.is_empty() {
        out.push_str(&format!("Cc: {}\r\n", cmd.cc.join(", ")));
    }
    // Bcc deliberately not included in headers — only in the SMTP RCPT
    // envelope. Lettre / smtp_conn handle that separately.
    out.push_str(&format!("Subject: {}\r\n", encode_subject_if_needed(&cmd.subject)));

    if let Some(parent) = &cmd.in_reply_to {
        out.push_str(&format!("In-Reply-To: {parent}\r\n"));
    }
    if !cmd.references.is_empty() {
        // RFC 5322: References is a space-separated list of Message-IDs.
        out.push_str(&format!("References: {}\r\n", cmd.references.join(" ")));
    }

    out.push_str("\r\n");
    out.push_str(&cmd.body);
    if !cmd.body.ends_with('\n') {
        out.push_str("\r\n");
    }
    out.into_bytes()
}

/// Format an offset-aware timestamp as RFC 2822 (`Date:` header).
fn rfc2822(dt: DateTime<Utc>) -> String {
    dt.format("%a, %d %b %Y %H:%M:%S %z").to_string()
}

/// If the subject contains non-ASCII, wrap it in a single MIME
/// "encoded-word" (RFC 2047) using base64 + UTF-8 — adequate for the
/// 48.4 scope. Multi-line / very-long subjects can wait for 48.5's
/// `mail-builder` integration.
fn encode_subject_if_needed(s: &str) -> String {
    if s.is_ascii() {
        s.to_string()
    } else {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        format!("=?utf-8?B?{b64}?=")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(subject: &str, body: &str) -> OutboundCommand {
        OutboundCommand {
            to: vec!["alice@x.com".into()],
            cc: vec![],
            bcc: vec![],
            subject: subject.into(),
            body: body.into(),
            in_reply_to: None,
            references: vec![],
            attachments: vec![],
        }
    }

    #[test]
    fn message_id_format_includes_domain_from_from() {
        let id = generate_message_id("ops@example.com");
        assert!(id.starts_with('<') && id.ends_with('>'));
        assert!(id.contains("@example.com>"));
        // ts_ms.uuid skeleton
        let body = id.trim_start_matches('<').trim_end_matches('>');
        let parts: Vec<&str> = body.split('@').collect();
        assert_eq!(parts.len(), 2);
        let stem: Vec<&str> = parts[0].split('.').collect();
        assert_eq!(stem.len(), 2);
        assert!(stem[0].chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn message_id_falls_back_to_nexo_when_from_malformed() {
        let id = generate_message_id("bare-username");
        assert!(id.contains("@nexo>"));
    }

    #[test]
    fn build_text_mime_contains_required_headers() {
        let now = Utc::now();
        let bytes = build_text_mime("<abc@x>", "ops@example.com", &cmd("Hi", "hello"), now);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("MIME-Version: 1.0\r\n"));
        assert!(s.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(s.contains("Message-ID: <abc@x>\r\n"));
        assert!(s.contains("From: ops@example.com\r\n"));
        assert!(s.contains("To: alice@x.com\r\n"));
        assert!(s.contains("Subject: Hi\r\n"));
        assert!(s.contains("\r\n\r\nhello\r\n"));
    }

    #[test]
    fn build_text_mime_omits_bcc_from_headers() {
        let mut c = cmd("Hi", "hello");
        c.bcc = vec!["secret@x.com".into()];
        let bytes = build_text_mime("<abc@x>", "ops@example.com", &c, Utc::now());
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(!s.contains("Bcc:"));
        assert!(!s.contains("secret@x.com"));
    }

    #[test]
    fn build_text_mime_writes_in_reply_to_and_references() {
        let mut c = cmd("Re: Hi", "yes");
        c.in_reply_to = Some("<root@x>".into());
        c.references = vec!["<root@x>".into(), "<reply1@x>".into()];
        let bytes = build_text_mime("<abc@x>", "ops@example.com", &c, Utc::now());
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("In-Reply-To: <root@x>\r\n"));
        assert!(s.contains("References: <root@x> <reply1@x>\r\n"));
    }

    #[test]
    fn build_text_mime_encodes_non_ascii_subject() {
        let bytes = build_text_mime(
            "<abc@x>",
            "ops@example.com",
            &cmd("¿Qué tal?", "ok"),
            Utc::now(),
        );
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("Subject: =?utf-8?B?"));
    }

    #[test]
    fn build_text_mime_appends_crlf_when_body_unterminated() {
        let bytes = build_text_mime("<abc@x>", "ops@example.com", &cmd("Hi", "no-eol"), Utc::now());
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.ends_with("\r\n"));
    }
}
