//! Outbound MIME builder (Phase 48.5).
//!
//! Replaces `mime_text.rs` (the 48.4 text-only path). Branches
//! internally:
//!
//! - **No attachments** → `text/plain; charset=utf-8` produced by the
//!   same hand-rolled writer as 48.4 so the wire format stays
//!   bit-for-bit compatible for the simple case.
//! - **One or more attachments** → `mail-builder` 0.4 emits a
//!   `multipart/mixed` envelope with the body as the first part and
//!   each attachment as a sibling. Inferred mime type via
//!   `mime_guess` when the caller didn't set one. Inline attachments
//!   (`disposition = Inline`) emit `Content-ID` so HTML bodies can
//!   reference them via `cid:`.
//!
//! `Bcc` is *deliberately* not written into headers in either branch.
//! Recipients live in the SMTP envelope (`MAIL FROM` / `RCPT TO`) so
//! Bcc readers see no peer addresses, exactly as 48.4 promised.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mail_builder::headers::address::Address as MbAddress;
use mail_builder::headers::message_id::MessageId;
use mail_builder::MessageBuilder;
use uuid::Uuid;

use crate::events::{AttachmentDisposition, OutboundAttachmentRef, OutboundCommand};

pub fn generate_message_id(from: &str) -> String {
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or("nexo");
    let domain = if domain.is_empty() { "nexo" } else { domain };
    let ts_ms = Utc::now().timestamp_millis();
    let uuid = Uuid::new_v4().simple();
    format!("<{ts_ms}.{uuid}@{domain}>")
}

pub struct BuildContext<'a> {
    pub message_id: &'a str,
    pub from: &'a str,
    pub now: DateTime<Utc>,
}

/// Build RFC 5322 bytes ready for SMTP `DATA`. Reads each
/// `OutboundAttachmentRef.data_path` from disk; missing files cause
/// the entire build to fail so the caller can early-DLQ.
pub async fn build_mime(ctx: BuildContext<'_>, cmd: &OutboundCommand) -> Result<Vec<u8>> {
    if cmd.attachments.is_empty() {
        return Ok(build_text_only(&ctx, cmd));
    }

    // Read attachments first so a missing file fails fast (before we
    // start composing). The dispatcher's `enqueue_command` will surface
    // this as ack `Failed`.
    let mut loaded: Vec<(OutboundAttachmentRef, Vec<u8>, String)> =
        Vec::with_capacity(cmd.attachments.len());
    for a in &cmd.attachments {
        let bytes = tokio::fs::read(&a.data_path)
            .await
            .with_context(|| format!("email/mime: read attachment {}", a.data_path))?;
        let mime_type = a.mime_type.clone().unwrap_or_else(|| {
            mime_guess::from_path(&a.filename)
                .first_raw()
                .unwrap_or("application/octet-stream")
                .to_string()
        });
        loaded.push((a.clone(), bytes, mime_type));
    }

    // Owned strings stripped of angle brackets — mail-builder adds
    // them on write, and the `MessageId::from(&[String])` impl needs
    // a slice that outlives the builder.
    let stripped_refs: Vec<String> = cmd
        .references
        .iter()
        .map(|s| strip_brackets(s).to_string())
        .collect();
    let stripped_msg_id = strip_brackets(ctx.message_id).to_string();
    let stripped_in_reply_to = cmd
        .in_reply_to
        .as_deref()
        .map(|s| strip_brackets(s).to_string());

    let mut builder = MessageBuilder::new()
        .from(MbAddress::new_address(None::<&str>, ctx.from))
        .to(addr_list(&cmd.to))
        .subject(cmd.subject.as_str())
        .message_id(MessageId::from(stripped_msg_id))
        .date(ctx.now.timestamp());

    if !cmd.cc.is_empty() {
        builder = builder.cc(addr_list(&cmd.cc));
    }
    if let Some(parent) = &stripped_in_reply_to {
        builder = builder.in_reply_to(MessageId::from(parent.clone()));
    }
    if !stripped_refs.is_empty() {
        builder = builder.references(MessageId::from(stripped_refs.as_slice()));
    }

    // text/plain body — `text_body` is the multipart-friendly setter.
    // Combined with `attachment`/`inline`, mail-builder wraps the
    // message in `multipart/mixed` automatically.
    builder = builder.text_body(cmd.body.as_str());

    for (att, bytes, mime_type) in loaded {
        match att.disposition {
            AttachmentDisposition::Inline => {
                let cid = att.content_id.clone().unwrap_or_else(|| {
                    // mail-builder requires a CID for inline parts; if
                    // the caller didn't pin one, synthesize a stable
                    // value from the filename so HTML can reference it.
                    format!("inline-{}", att.filename)
                });
                builder = builder.inline(mime_type, cid, bytes);
            }
            AttachmentDisposition::Attachment => {
                builder = builder.attachment(mime_type, att.filename, bytes);
            }
        }
    }

    let raw = builder.write_to_vec().context("email/mime: write_to_vec")?;
    Ok(raw)
}

fn addr_list<'a>(items: &'a [String]) -> Vec<MbAddress<'a>> {
    items
        .iter()
        .map(|s| MbAddress::new_address(None::<&str>, s.as_str()))
        .collect()
}

/// `mail-builder`'s `MessageId::from(s)` expects the bare value (no
/// angle brackets) — it adds the brackets on write.
fn strip_brackets(s: &str) -> &str {
    s.trim().trim_start_matches('<').trim_end_matches('>')
}

/// 48.4-equivalent text-only writer: keeps the wire format identical
/// for the no-attachment case so existing callers / wire-format
/// expectations don't shift.
fn build_text_only(ctx: &BuildContext, cmd: &OutboundCommand) -> Vec<u8> {
    let mut out = String::with_capacity(256 + cmd.body.len());
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    out.push_str("Content-Transfer-Encoding: 8bit\r\n");
    out.push_str(&format!("Date: {}\r\n", rfc2822(ctx.now)));
    out.push_str(&format!("Message-ID: {}\r\n", ctx.message_id));
    out.push_str(&format!("From: {}\r\n", ctx.from));
    if !cmd.to.is_empty() {
        out.push_str(&format!("To: {}\r\n", cmd.to.join(", ")));
    }
    if !cmd.cc.is_empty() {
        out.push_str(&format!("Cc: {}\r\n", cmd.cc.join(", ")));
    }
    out.push_str(&format!(
        "Subject: {}\r\n",
        encode_subject_if_needed(&cmd.subject)
    ));
    if let Some(parent) = &cmd.in_reply_to {
        out.push_str(&format!("In-Reply-To: {parent}\r\n"));
    }
    if !cmd.references.is_empty() {
        out.push_str(&format!("References: {}\r\n", cmd.references.join(" ")));
    }
    out.push_str("\r\n");
    out.push_str(&cmd.body);
    if !cmd.body.ends_with('\n') {
        out.push_str("\r\n");
    }
    out.into_bytes()
}

fn rfc2822(dt: DateTime<Utc>) -> String {
    dt.format("%a, %d %b %Y %H:%M:%S %z").to_string()
}

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

    fn cmd_no_attach(subject: &str, body: &str) -> OutboundCommand {
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

    fn ctx() -> BuildContext<'static> {
        BuildContext {
            message_id: "<abc@x>",
            from: "ops@example.com",
            now: chrono::DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    #[tokio::test]
    async fn no_attachments_matches_48_4_wire() {
        let bytes = build_mime(ctx(), &cmd_no_attach("Hi", "hello"))
            .await
            .unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(s.contains("Message-ID: <abc@x>\r\n"));
        assert!(s.contains("From: ops@example.com\r\n"));
        assert!(s.contains("To: alice@x.com\r\n"));
        assert!(s.contains("Subject: Hi\r\n"));
        assert!(s.contains("\r\n\r\nhello\r\n"));
        assert!(!s.contains("multipart"));
    }

    #[tokio::test]
    async fn message_id_format_includes_domain_from_from() {
        let id = generate_message_id("ops@example.com");
        assert!(id.contains("@example.com>"));
    }

    #[tokio::test]
    async fn message_id_falls_back_to_nexo_when_from_malformed() {
        let id = generate_message_id("bare-username");
        assert!(id.contains("@nexo>"));
    }

    #[tokio::test]
    async fn no_attachments_omits_bcc_from_headers() {
        let mut c = cmd_no_attach("Hi", "hello");
        c.bcc = vec!["secret@x.com".into()];
        let bytes = build_mime(ctx(), &c).await.unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(!s.contains("Bcc:"));
        assert!(!s.contains("secret@x.com"));
    }

    #[tokio::test]
    async fn no_attachments_writes_in_reply_to_and_references() {
        let mut c = cmd_no_attach("Re: Hi", "yes");
        c.in_reply_to = Some("<root@x>".into());
        c.references = vec!["<root@x>".into(), "<reply1@x>".into()];
        let bytes = build_mime(ctx(), &c).await.unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("In-Reply-To: <root@x>\r\n"));
        assert!(s.contains("References: <root@x> <reply1@x>\r\n"));
    }

    #[tokio::test]
    async fn no_attachments_encodes_non_ascii_subject() {
        let bytes = build_mime(ctx(), &cmd_no_attach("¿Qué tal?", "ok"))
            .await
            .unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("Subject: =?utf-8?B?"));
    }

    #[tokio::test]
    async fn with_attachment_emits_multipart_mixed() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("report.pdf");
        std::fs::write(&pdf, b"%PDF-1.4 stub").unwrap();
        let cmd = OutboundCommand {
            to: vec!["alice@x.com".into()],
            cc: vec![],
            bcc: vec![],
            subject: "With PDF".into(),
            body: "see attached".into(),
            in_reply_to: None,
            references: vec![],
            attachments: vec![OutboundAttachmentRef {
                data_path: pdf.to_string_lossy().into_owned(),
                filename: "report.pdf".into(),
                mime_type: None,
                content_id: None,
                disposition: AttachmentDisposition::Attachment,
            }],
        };
        let bytes = build_mime(ctx(), &cmd).await.unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("multipart/mixed"), "expected multipart in:\n{s}");
        assert!(
            s.contains("application/pdf"),
            "expected mime_guess pdf:\n{s}"
        );
        assert!(s.contains("filename"));
        assert!(s.contains("report.pdf"));
        // Body part still present.
        assert!(s.contains("see attached"));
    }

    #[tokio::test]
    async fn missing_attachment_file_errors() {
        let cmd = OutboundCommand {
            to: vec!["a@x".into()],
            cc: vec![],
            bcc: vec![],
            subject: "Hi".into(),
            body: "ok".into(),
            in_reply_to: None,
            references: vec![],
            attachments: vec![OutboundAttachmentRef {
                data_path: "/tmp/definitely-does-not-exist-48-5".into(),
                filename: "ghost.bin".into(),
                mime_type: None,
                content_id: None,
                disposition: AttachmentDisposition::Attachment,
            }],
        };
        let r = build_mime(ctx(), &cmd).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn with_attachment_explicit_mime_overrides_guess() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("data.bin");
        std::fs::write(&p, b"\x00\x01\x02").unwrap();
        let cmd = OutboundCommand {
            to: vec!["a@x".into()],
            cc: vec![],
            bcc: vec![],
            subject: "Hi".into(),
            body: "ok".into(),
            in_reply_to: None,
            references: vec![],
            attachments: vec![OutboundAttachmentRef {
                data_path: p.to_string_lossy().into_owned(),
                filename: "data.bin".into(),
                mime_type: Some("application/x-custom".into()),
                content_id: None,
                disposition: AttachmentDisposition::Attachment,
            }],
        };
        let bytes = build_mime(ctx(), &cmd).await.unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("application/x-custom"));
    }
}
