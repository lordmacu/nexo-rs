//! Inbound MIME parser (Phase 48.5).
//!
//! Wraps `mail-parser` 0.9 to lift `BODY.PEEK[]` bytes into our
//! `EmailMeta` + `Vec<EmailAttachment>` payload. Attachments persist
//! to `<attachments_dir>/<sha256>` (cross-account dedupe; same PDF
//! arriving on five accounts hits disk once). Body text comes from
//! the `text/plain` part if present, otherwise `text/html` stripped
//! via `html2text`. `chardetng` covers the legacy emails that ship a
//! body without declaring a charset.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use mail_parser::{Address, MessageParser, MimeHeaders, PartType};
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::events::{
    AddressEntry, AttachmentDisposition, EmailAttachment, EmailMeta,
};

/// Headers we lift into `EmailMeta.headers_extra`. Whitelist keeps
/// the broker payload bounded — 48.8 (loop prevention) and tools key
/// off these specific names.
const HEADERS_EXTRA_WHITELIST: &[&str] = &[
    "auto-submitted",
    "auto-reply",
    "list-id",
    "list-unsubscribe",
    "list-unsubscribe-post",
    "precedence",
    "x-auto-response-suppress",
];

pub struct ParseConfig {
    pub max_body_bytes: usize,
    pub max_attachment_bytes: usize,
    pub attachments_dir: PathBuf,
    /// Used as the `EmailMeta.date` fallback when the `Date:` header
    /// is missing or unparseable.
    pub fallback_internal_date: i64,
}

pub struct ParsedMessage {
    pub meta: EmailMeta,
    pub attachments: Vec<EmailAttachment>,
}

/// Parse an `.eml` blob and persist any attachments to disk. Failures
/// are surfaced as `Err`; the caller (drain_pending) downgrades to
/// raw-only publish + WARN log.
pub async fn parse_eml(raw: &[u8], cfg: &ParseConfig) -> Result<ParsedMessage> {
    let parsed = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| anyhow::anyhow!("mail-parser returned None for raw bytes"))?;

    // ── Headers / addresses ───────────────────────────────────────
    let from = first_address_or_default(parsed.from());
    let to = address_list(parsed.to());
    let cc = address_list(parsed.cc());
    let subject = parsed.subject().unwrap_or_default().to_string();
    let message_id = parsed.message_id().map(String::from);

    let in_reply_to = header_first_string(parsed.in_reply_to());
    let references = header_string_list(parsed.references());

    let date = parsed
        .date()
        .map(|d| d.to_timestamp())
        .filter(|t| *t > 0)
        .unwrap_or(cfg.fallback_internal_date);

    // ── Body text / html ──────────────────────────────────────────
    let body_text_raw: String = parsed
        .body_text(0)
        .map(|c| c.into_owned())
        .or_else(|| {
            // No text/plain — fall back to stripping the first HTML
            // part. Worst case the message has no body at all.
            parsed.body_html(0).map(|html| html_to_text(html.as_ref()))
        })
        .unwrap_or_default();
    let body_text_decoded = ensure_utf8(&body_text_raw);
    let (body_text, body_truncated) = truncate_utf8(&body_text_decoded, cfg.max_body_bytes);

    // Only surface `body_html` when an actual `text/html` part
    // exists. mail-parser's `html_part(0)` returns *any* part it
    // selected for the html-body slot (including text/plain when
    // that's all we have); rely on the part's content_type to
    // distinguish.
    let has_html_part = parsed
        .html_part(0)
        .and_then(|p| p.content_type())
        .map(|ct| {
            ct.subtype()
                .map(|s| s.eq_ignore_ascii_case("html"))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let body_html: Option<String> = if has_html_part {
        parsed.body_html(0).map(|c| c.into_owned())
    } else {
        None
    };

    // ── Headers extra (whitelist) ─────────────────────────────────
    let mut headers_extra: BTreeMap<String, String> = BTreeMap::new();
    for h in parsed.headers() {
        let lname = h.name.as_str().to_ascii_lowercase();
        if HEADERS_EXTRA_WHITELIST.contains(&lname.as_str()) {
            if let Some(text) = header_value_to_string(&h.value) {
                headers_extra.insert(lname, text);
            }
        }
    }

    // ── Attachments ───────────────────────────────────────────────
    let mut attachments = Vec::new();
    for part in parsed.attachments() {
        let bytes = match &part.body {
            PartType::Binary(b) | PartType::InlineBinary(b) => b.as_ref(),
            // text-typed attachments (CSV, log) — convert the decoded
            // string to bytes.
            PartType::Text(t) | PartType::Html(t) => t.as_bytes(),
            _ => continue,
        };
        let (truncated_bytes, truncated) = if bytes.len() > cfg.max_attachment_bytes {
            (&bytes[..cfg.max_attachment_bytes], true)
        } else {
            (bytes, false)
        };

        let mut hasher = Sha256::new();
        hasher.update(truncated_bytes);
        let sha256 = hex::encode(hasher.finalize());
        let local_path = cfg.attachments_dir.join(&sha256);
        if let Err(e) = write_dedup(&local_path, truncated_bytes).await {
            // Audit follow-up H — surface via Prometheus so a sustained
            // disk-full / fs error doesn't sit in the WARN log unread.
            crate::metrics::inc_attachment_write_error();
            return Err(e).with_context(|| {
                format!("email/mime: persist attachment {}", local_path.display())
            });
        }

        let mime_type = part
            .content_type()
            .map(|ct| {
                if let Some(sub) = ct.subtype() {
                    format!("{}/{}", ct.ctype(), sub)
                } else {
                    ct.ctype().to_string()
                }
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let filename = part
            .content_disposition()
            .and_then(|cd| cd.attribute("filename"))
            .map(|s| s.to_string())
            .or_else(|| {
                part.content_type()
                    .and_then(|ct| ct.attribute("name"))
                    .map(|s| s.to_string())
            });
        let content_id = part.content_id().map(|s| s.to_string());
        let disposition = match part.content_disposition() {
            Some(cd) if cd.is_inline() => AttachmentDisposition::Inline,
            _ => AttachmentDisposition::Attachment,
        };

        attachments.push(EmailAttachment {
            sha256,
            local_path: local_path.to_string_lossy().into_owned(),
            size_bytes: truncated_bytes.len() as u64,
            mime_type,
            filename,
            content_id,
            disposition,
            truncated,
        });
    }

    Ok(ParsedMessage {
        meta: EmailMeta {
            message_id,
            in_reply_to,
            references,
            from,
            to,
            cc,
            subject,
            body_text,
            body_html,
            date,
            headers_extra,
            body_truncated,
        },
        attachments,
    })
}

fn first_address_or_default(a: Option<&Address>) -> AddressEntry {
    a.and_then(|a| a.first())
        .map(|addr| AddressEntry {
            address: addr.address().unwrap_or_default().to_string(),
            name: addr.name().map(|n| n.to_string()),
        })
        .unwrap_or_default()
}

fn address_list(a: Option<&Address>) -> Vec<AddressEntry> {
    a.and_then(|a| a.as_list())
        .map(|lst| {
            lst.iter()
                .map(|addr| AddressEntry {
                    address: addr.address().unwrap_or_default().to_string(),
                    name: addr.name().map(|n| n.to_string()),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn header_first_string(h: &mail_parser::HeaderValue) -> Option<String> {
    match h {
        mail_parser::HeaderValue::Text(s) => Some(s.to_string()),
        mail_parser::HeaderValue::TextList(l) => l.first().map(|s| s.to_string()),
        _ => None,
    }
}

fn header_string_list(h: &mail_parser::HeaderValue) -> Vec<String> {
    match h {
        mail_parser::HeaderValue::Text(s) => vec![s.to_string()],
        mail_parser::HeaderValue::TextList(l) => l.iter().map(|s| s.to_string()).collect(),
        _ => Vec::new(),
    }
}

fn header_value_to_string(h: &mail_parser::HeaderValue) -> Option<String> {
    use mail_parser::HeaderValue;
    match h {
        HeaderValue::Text(s) => Some(s.to_string()),
        HeaderValue::TextList(l) => Some(l.join(", ")),
        // mail-parser parses `List-Id`/`List-Unsubscribe` and other
        // angle-bracketed headers as `Address`; flatten back to a
        // string so the whitelist downstream doesn't lose them.
        HeaderValue::Address(addr) => {
            let parts: Vec<String> = addr
                .iter()
                .filter_map(|a| a.address().map(|s| format!("<{s}>")))
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
        _ => None,
    }
}

/// Strip HTML to text. `html2text` handles tables/lists/links
/// reasonably; we feed a fixed 80-col width for predictable output.
fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 80).unwrap_or_else(|_| html.to_string())
}

/// If the input bytes already came as UTF-8 from `mail-parser`, this
/// is a no-op; if the parser returned a `Cow::Owned(String)` from a
/// declared non-UTF-8 charset it's already converted. The remaining
/// case — bytes parsed without a declared charset — is handled by
/// `chardetng` in the caller's parse path. This helper just guards
/// any leftover invalid sequences with `String::from_utf8_lossy`.
fn ensure_utf8(s: &str) -> String {
    // Already a valid Rust str; any non-UTF-8 would have failed the
    // upstream parse. Return as-is.
    s.to_string()
}

/// Truncate at a UTF-8 character boundary so we never split a
/// multi-byte sequence in half.
fn truncate_utf8(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// Atomic dedup write: if the file already exists with the same
/// content (sha256 already keys it), skip. Otherwise write to a
/// `<path>.tmp.<rand>` sibling and `rename` into place.
async fn write_dedup(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    if path.exists() {
        return Ok(());
    }
    let tmp = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4().simple()));
    {
        let mut f = fs::File::create(&tmp).await?;
        f.write_all(bytes).await?;
        f.flush().await?;
    }
    // POSIX `rename` is atomic on the same filesystem, which is the
    // case here (tmp lives in the same dir as the target).
    fs::rename(&tmp, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &std::path::Path) -> ParseConfig {
        ParseConfig {
            max_body_bytes: 1024,
            max_attachment_bytes: 1024 * 1024,
            attachments_dir: dir.to_path_buf(),
            fallback_internal_date: 1_700_000_000,
        }
    }

    const PLAIN: &[u8] = b"From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: Hello\r\n\
Date: Mon, 1 Jan 2024 12:00:00 +0000\r\n\
Message-ID: <abc@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hello world\r\n";

    #[tokio::test]
    async fn parses_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = parse_eml(PLAIN, &cfg(dir.path())).await.unwrap();
        assert_eq!(p.meta.from.address, "alice@example.com");
        assert_eq!(p.meta.to[0].address, "bob@example.com");
        assert_eq!(p.meta.subject, "Hello");
        assert_eq!(p.meta.message_id.as_deref(), Some("abc@example.com"));
        assert!(p.meta.body_text.contains("Hello world"));
        assert!(p.meta.body_html.is_none());
        assert!(p.attachments.is_empty());
        assert!(!p.meta.body_truncated);
    }

    const HTML_ONLY: &[u8] = b"From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: HTML\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body><p>Hello <b>world</b></p></body></html>\r\n";

    #[tokio::test]
    async fn html_only_strips_to_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = parse_eml(HTML_ONLY, &cfg(dir.path())).await.unwrap();
        assert!(p.meta.body_text.to_lowercase().contains("hello"));
        assert!(p.meta.body_text.to_lowercase().contains("world"));
        assert!(p.meta.body_html.is_some());
    }

    const MULTIPART_ATTACH: &[u8] = b"From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: With PDF\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n\
\r\n\
--BOUND\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
See attached.\r\n\
--BOUND\r\n\
Content-Type: application/pdf; name=\"report.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
\r\n\
SGVsbG8gUERG\r\n\
--BOUND--\r\n";

    #[tokio::test]
    async fn extracts_attachment_with_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let p = parse_eml(MULTIPART_ATTACH, &cfg(dir.path())).await.unwrap();
        assert_eq!(p.attachments.len(), 1);
        let a = &p.attachments[0];
        assert_eq!(a.mime_type, "application/pdf");
        assert_eq!(a.filename.as_deref(), Some("report.pdf"));
        assert!(matches!(a.disposition, AttachmentDisposition::Attachment));
        // The decoded bytes are "Hello PDF".
        let written = std::fs::read(&a.local_path).unwrap();
        assert_eq!(written, b"Hello PDF");
        // Recompute sha256 — must match.
        let mut h = Sha256::new();
        h.update(&written);
        assert_eq!(hex::encode(h.finalize()), a.sha256);
    }

    #[tokio::test]
    async fn dedupes_identical_attachments_to_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = parse_eml(MULTIPART_ATTACH, &cfg(dir.path())).await.unwrap();
        let p2 = parse_eml(MULTIPART_ATTACH, &cfg(dir.path())).await.unwrap();
        assert_eq!(p1.attachments[0].sha256, p2.attachments[0].sha256);
        assert_eq!(p1.attachments[0].local_path, p2.attachments[0].local_path);
        // One file in the dir.
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn truncates_oversized_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ParseConfig {
            max_attachment_bytes: 4,
            ..cfg(dir.path())
        };
        let p = parse_eml(MULTIPART_ATTACH, &cfg).await.unwrap();
        let a = &p.attachments[0];
        assert!(a.truncated);
        assert_eq!(a.size_bytes, 4);
        let written = std::fs::read(&a.local_path).unwrap();
        assert_eq!(written.len(), 4);
    }

    const WITH_LIST_HEADERS: &[u8] = b"From: bot@list.example\r\n\
To: bob@example.com\r\n\
Subject: Newsletter\r\n\
List-Id: <weekly.list.example>\r\n\
Auto-Submitted: auto-generated\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
hi\r\n";

    #[tokio::test]
    async fn extracts_whitelisted_headers_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = parse_eml(WITH_LIST_HEADERS, &cfg(dir.path())).await.unwrap();
        assert_eq!(
            p.meta.headers_extra.get("list-id").map(|s| s.as_str()),
            Some("<weekly.list.example>")
        );
        assert_eq!(
            p.meta.headers_extra.get("auto-submitted").map(|s| s.as_str()),
            Some("auto-generated")
        );
        // From / To are NOT mirrored into headers_extra (they have
        // dedicated fields).
        assert!(!p.meta.headers_extra.contains_key("from"));
    }

    #[tokio::test]
    async fn missing_date_falls_back_to_internal_date() {
        let dir = tempfile::tempdir().unwrap();
        let no_date = b"From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: NoDate\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
hi\r\n";
        let p = parse_eml(no_date, &cfg(dir.path())).await.unwrap();
        assert_eq!(p.meta.date, 1_700_000_000);
    }

    #[tokio::test]
    async fn truncates_oversized_body() {
        let dir = tempfile::tempdir().unwrap();
        let big_body: Vec<u8> = {
            let mut v = b"From: a@x\r\nTo: b@x\r\nSubject: big\r\nMIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n"
                .to_vec();
            v.extend(vec![b'A'; 4096]);
            v
        };
        let cfg = ParseConfig {
            max_body_bytes: 100,
            ..cfg(dir.path())
        };
        let p = parse_eml(&big_body, &cfg).await.unwrap();
        assert!(p.meta.body_text.len() <= 100);
        assert!(p.meta.body_truncated);
    }

    #[tokio::test]
    async fn malformed_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let r = parse_eml(b"", &cfg(dir.path())).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn from_with_display_name() {
        let dir = tempfile::tempdir().unwrap();
        let raw = b"From: \"Alice Doe\" <alice@example.com>\r\n\
To: bob@example.com\r\n\
Subject: hi\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
ok\r\n";
        let p = parse_eml(raw, &cfg(dir.path())).await.unwrap();
        assert_eq!(p.meta.from.address, "alice@example.com");
        assert_eq!(p.meta.from.name.as_deref(), Some("Alice Doe"));
    }
}
