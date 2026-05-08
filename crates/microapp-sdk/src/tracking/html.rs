//! HTML helpers — pure functions that mutate outbound bodies
//! to thread engagement signals back through the ingest route.
//!
//! Two operations:
//!
//! 1. [`inject_pixel`] — appends a `<img>` 1×1 GIF tag right
//!    before the closing `</body>` (or at the end of the
//!    document when no body close tag is present). The tag
//!    has `width=1 height=1 alt=""` so accessibility tools
//!    skip it; the `src` is the operator-supplied pixel URL
//!    (already HMAC-signed by the caller).
//! 2. [`rewrite_links`] — finds every `<a href="X">` whose
//!    `X` looks like a tracked URL (http/https + non-empty
//!    path), generates a stable `LinkId`, replaces the `href`
//!    with the click redirector URL, and yields the
//!    `(link_id ↔ X)` mapping so the caller persists it for
//!    later resolution.
//!
//! ## Why regex not a DOM parser
//!
//! A full HTML parser would catch every edge case (anchors
//! split across lines, `href` spelled `HREF`, …) at the cost
//! of ~5 MB of dependencies. Outbound emails are operator-
//! authored or template-driven — both produce tidy HTML where
//! a regex is good enough. Future opt-in: a `dom-rewrite`
//! feature gating an `html5ever` impl.
//!
//! Edge cases the regex DOES handle:
//!
//! - Single + double-quoted href values.
//! - Mixed-case `<A HREF=...>` (case-insensitive).
//! - Internal anchors (`#section`) — left as-is.
//! - `mailto:` / `tel:` / `javascript:` — left as-is.
//! - Whitespace around `=`.
//!
//! Edge cases NOT handled (caller's responsibility):
//!
//! - Anchors inside `<style>` / `<script>` (no realistic email
//!   client renders these clickably).
//! - Multi-line `<a>` tags (operator-authored HTML is single
//!   line per tag in practice).

use std::sync::OnceLock;

use regex::Regex;

use super::token::TrackingTokenSigner;
use super::types::{LinkId, LinkMapping, MsgId};

/// Cached anchor regex. `OnceLock` so we compile once per
/// process. Pattern: `<a` (case-insens) → any attrs → `href` →
/// optional whitespace → `=` → optional ws → quoted value.
fn anchor_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)(<a\b[^>]*?\bhref\s*=\s*)(?:"([^"]*)"|'([^']*)')([^>]*>)"#,
        )
        .expect("anchor regex compiles")
    })
}

/// Append the open-pixel tag to the HTML body. Idempotent —
/// calling twice on the same body produces two tags (caller
/// must call exactly once per outbound).
///
/// `pixel_url` is the full URL the ingest route serves (already
/// HMAC-signed by the caller).
pub fn inject_pixel(html: &str, pixel_url: &str) -> String {
    let tag = format!(
        r#"<img src="{}" width="1" height="1" alt="" style="display:block;border:0;" />"#,
        html_attr_escape(pixel_url),
    );
    if let Some(idx) = ci_rfind(html, "</body>") {
        // Slot the tag right before the closing body so the
        // browser actually downloads the pixel before unloading.
        let mut out = String::with_capacity(html.len() + tag.len());
        out.push_str(&html[..idx]);
        out.push_str(&tag);
        out.push_str(&html[idx..]);
        out
    } else {
        // No </body> — naked HTML fragment. Append at the end
        // so a follow-up `inject_pixel` on the same fragment
        // doesn't shift the original payload.
        let mut out = String::with_capacity(html.len() + tag.len());
        out.push_str(html);
        out.push_str(&tag);
        out
    }
}

/// Outcome of [`rewrite_links`]: rewritten HTML + the link
/// map the caller persists. Caller writes one row per entry to
/// the tracking store so the click redirector can resolve
/// `link_id` → `original_url` at request time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteOutcome {
    /// Rewritten HTML body — every clickable anchor `href`
    /// swapped for the click-redirector URL.
    pub html: String,
    /// One entry per rewritten anchor. Caller persists these
    /// rows so the redirector can resolve `link_id` to the
    /// original `href` at click time.
    pub mappings: Vec<LinkMapping>,
}

/// Rewrite every clickable anchor `href` to the tracked
/// redirector URL. Returns the rewritten body + the map
/// (`link_id`, `original_url`) the caller persists.
///
/// `base_url` — operator's public base URL (e.g.
/// `https://track.acme.com`); the redirector is built as
/// `{base_url}/t/c/{tenant_id}/{msg_id}/{link_id}?tag={hmac}`.
///
/// Skips anchors whose href is:
/// - Empty
/// - Internal (`#section`)
/// - Non-HTTP scheme (`mailto:`, `tel:`, `javascript:`,
///   `data:`)
/// - Already pointing at the redirector path (idempotent
///   re-rewrite avoidance).
///
/// `link_ids` are sequential `L0`, `L1`, …  — short enough
/// for URL aesthetics + stable enough for analytics joins.
pub fn rewrite_links(
    html: &str,
    base_url: &str,
    tenant_id: &str,
    msg_id: &MsgId,
    signer: &TrackingTokenSigner,
) -> RewriteOutcome {
    let re = anchor_regex();
    let mut mappings: Vec<LinkMapping> = Vec::new();
    let mut next_id: u32 = 0;
    let trimmed_base = base_url.trim_end_matches('/').to_string();

    let rewritten = re.replace_all(html, |caps: &regex::Captures<'_>| {
        let prefix = caps.get(1).map_or("", |m| m.as_str());
        let original = caps
            .get(2)
            .or_else(|| caps.get(3))
            .map_or("", |m| m.as_str());
        let suffix = caps.get(4).map_or("", |m| m.as_str());

        if !is_trackable_href(original, &trimmed_base, tenant_id) {
            return caps.get(0).map_or(String::new(), |m| m.as_str().to_string());
        }
        let link_id = LinkId(format!("L{next_id}"));
        next_id += 1;
        let token = signer.sign_click(tenant_id, msg_id, &link_id);
        let redir = format!(
            "{base}/t/c/{tenant}/{msg}/{lid}?tag={tag}",
            base = trimmed_base,
            tenant = url_path_escape(tenant_id),
            msg = url_path_escape(msg_id.as_str()),
            lid = url_path_escape(link_id.as_str()),
            tag = url_query_escape(token.as_str()),
        );
        mappings.push(LinkMapping {
            link_id,
            original_url: original.to_string(),
        });
        format!(r#"{prefix}"{redir}"{suffix}"#)
    });

    RewriteOutcome {
        html: rewritten.into_owned(),
        mappings,
    }
}

fn is_trackable_href(href: &str, base_url: &str, tenant_id: &str) -> bool {
    if href.is_empty() {
        return false;
    }
    if href.starts_with('#') {
        return false;
    }
    let lower = href.to_ascii_lowercase();
    // Only http(s) — `mailto`, `tel`, `javascript`, `data` are
    // not click-trackable.
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return false;
    }
    // Idempotency — don't rewrite a link that already points at
    // our redirector for the same tenant.
    let prefix = format!("{}/t/c/{}/", base_url, tenant_id);
    if href.starts_with(&prefix) {
        return false;
    }
    true
}

fn html_attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn url_path_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if matches!(
            b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~'
        ) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn url_query_escape(s: &str) -> String {
    // Token is base64url already — no chars need escaping.
    // Keep the function around for symmetry + future-proofing.
    url_path_escape(s)
}

/// Case-insensitive `rfind` for ASCII needles. Cheaper than a
/// full lower-cased copy of the haystack — we only need this
/// for `</body>` and `</BODY>` etc.
fn ci_rfind(haystack: &str, needle: &str) -> Option<usize> {
    let needle_lower = needle.to_ascii_lowercase();
    haystack
        .char_indices()
        .rev()
        .find_map(|(i, _)| {
            haystack
                .get(i..i + needle.len())
                .filter(|s| s.eq_ignore_ascii_case(&needle_lower))
                .map(|_| i)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> TrackingTokenSigner {
        TrackingTokenSigner::new(vec![0u8; 32]).unwrap()
    }

    // ─── inject_pixel ─────────────────────────────────────────

    #[test]
    fn inject_pixel_before_body_close() {
        let html = "<html><body>hi</body></html>";
        let out = inject_pixel(html, "https://t.example/x");
        assert!(out.contains("<img"));
        assert!(out.contains("https://t.example/x"));
        // Pixel must precede the closing body.
        let img_idx = out.find("<img").unwrap();
        let body_idx = out.find("</body>").unwrap();
        assert!(img_idx < body_idx);
    }

    #[test]
    fn inject_pixel_appends_when_no_body_tag() {
        let html = "<p>hello</p>";
        let out = inject_pixel(html, "https://t.example/x");
        assert!(out.starts_with("<p>hello</p>"));
        assert!(out.ends_with("/>"));
    }

    #[test]
    fn inject_pixel_handles_mixed_case_body() {
        let html = "<HTML><BODY>x</BODY></HTML>";
        let out = inject_pixel(html, "https://t.example/x");
        // Pixel slotted before the closing tag regardless of case.
        let img_idx = out.find("<img").unwrap();
        let body_idx = out.to_ascii_lowercase().find("</body>").unwrap();
        assert!(img_idx < body_idx);
    }

    #[test]
    fn inject_pixel_html_escapes_url() {
        let html = "<body>x</body>";
        let out = inject_pixel(html, "https://t.example/x?a=1&b=2");
        // The `&` inside the URL must be entity-escaped so the
        // mail client doesn't choke.
        assert!(out.contains("&amp;b=2"));
        assert!(!out.contains("?a=1&b=2"));
    }

    // ─── rewrite_links ────────────────────────────────────────

    #[test]
    fn rewrite_links_swaps_http_anchors() {
        let html = r#"<a href="https://acme.com/pricing">price</a>"#;
        let r = rewrite_links(html, "https://t.example", "acme",
            &MsgId::new("m1"), &signer());
        assert_eq!(r.mappings.len(), 1);
        assert_eq!(r.mappings[0].original_url, "https://acme.com/pricing");
        assert!(r.html.contains("/t/c/acme/m1/L0?tag="));
        assert!(!r.html.contains("https://acme.com/pricing"));
    }

    #[test]
    fn rewrite_links_assigns_sequential_ids() {
        let html = r#"<a href="https://a.com/">a</a> <a href="https://b.com/">b</a>"#;
        let r = rewrite_links(html, "https://t.example", "acme",
            &MsgId::new("m1"), &signer());
        assert_eq!(r.mappings.len(), 2);
        assert_eq!(r.mappings[0].link_id.as_str(), "L0");
        assert_eq!(r.mappings[1].link_id.as_str(), "L1");
    }

    #[test]
    fn rewrite_links_skips_internal_anchors() {
        // Note: needs `r##"..."##` because the body contains `"#`
        // which would close a `r#"..."#` raw literal early.
        let html = r##"<a href="#section">jump</a>"##;
        let r = rewrite_links(html, "https://t.example", "acme",
            &MsgId::new("m1"), &signer());
        assert_eq!(r.mappings.len(), 0);
        assert_eq!(r.html, html);
    }

    #[test]
    fn rewrite_links_skips_mailto_tel_js() {
        for href in ["mailto:x@y", "tel:+1", "javascript:void(0)", "data:text"]
        {
            let html = format!(r#"<a href="{href}">x</a>"#);
            let r = rewrite_links(&html, "https://t.example", "acme",
                &MsgId::new("m1"), &signer());
            assert_eq!(r.mappings.len(), 0, "should skip {href}");
            assert_eq!(r.html, html, "should not modify {href}");
        }
    }

    #[test]
    fn rewrite_links_idempotent_on_already_redirector() {
        let html = r#"<a href="https://t.example/t/c/acme/m1/L0?tag=abc">x</a>"#;
        let r = rewrite_links(html, "https://t.example", "acme",
            &MsgId::new("m1"), &signer());
        // Already pointing at our redirector — leave alone.
        assert_eq!(r.mappings.len(), 0);
    }

    #[test]
    fn rewrite_links_handles_single_quoted_href() {
        let html = r#"<a href='https://acme.com/'>x</a>"#;
        let r = rewrite_links(html, "https://t.example", "acme",
            &MsgId::new("m1"), &signer());
        assert_eq!(r.mappings.len(), 1);
    }

    #[test]
    fn rewrite_links_case_insensitive_tag() {
        let html = r#"<A HREF="https://acme.com/">x</A>"#;
        let r = rewrite_links(html, "https://t.example", "acme",
            &MsgId::new("m1"), &signer());
        assert_eq!(r.mappings.len(), 1);
    }

    #[test]
    fn rewrite_links_signed_token_is_url_safe() {
        let html = r#"<a href="https://acme.com/">x</a>"#;
        let r = rewrite_links(html, "https://t.example", "acme",
            &MsgId::new("m1"), &signer());
        let url = &r.html;
        assert!(url.contains("?tag="));
        // base64url tag is 22 chars after `?tag=`.
        let tag_pos = url.find("?tag=").unwrap() + "?tag=".len();
        let tail = &url[tag_pos..];
        let tag_end = tail.find(|c| c == '"' || c == '<').unwrap();
        let tag = &tail[..tag_end];
        assert_eq!(tag.len(), 22);
        assert!(!tag.contains('+'));
        assert!(!tag.contains('/'));
        assert!(!tag.contains('='));
    }

    #[test]
    fn rewrite_links_path_escapes_tenant_and_msg() {
        let html = r#"<a href="https://acme.com/">x</a>"#;
        // tenant_id with chars that need escaping (slash forbidden
        // by tenant validator but +, space, etc. can show up).
        let r = rewrite_links(html, "https://t.example", "acme corp",
            &MsgId::new("msg/1"), &signer());
        assert!(r.html.contains("/t/c/acme%20corp/msg%2F1/L0"));
    }

    #[test]
    fn rewrite_links_preserves_anchor_attributes() {
        let html = r#"<a href="https://acme.com/" target="_blank" rel="noopener">go</a>"#;
        let r = rewrite_links(html, "https://t.example", "acme",
            &MsgId::new("m1"), &signer());
        // Pre + post attributes survive.
        assert!(r.html.contains("target=\"_blank\""));
        assert!(r.html.contains("rel=\"noopener\""));
    }

    #[test]
    fn rewrite_links_strips_trailing_slash_from_base_url() {
        let html = r#"<a href="https://acme.com/">x</a>"#;
        let r = rewrite_links(html, "https://t.example/", "acme",
            &MsgId::new("m1"), &signer());
        // No double-slash in the rewritten URL.
        assert!(!r.html.contains("https://t.example//t/c/"));
        assert!(r.html.contains("https://t.example/t/c/"));
    }
}
