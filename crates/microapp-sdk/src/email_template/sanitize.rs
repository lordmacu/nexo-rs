//! Email-safe HTML sanitizer for the rich-text fields on
//! Heading + Paragraph blocks. Whitelists a tiny set of inline
//! formatting tags + attributes; strips everything else
//! (including scripts, styles, classes, event handlers).
//!
//! Why a custom sanitizer instead of `ammonia`: the whitelist is
//! tiny (~6 tags), the allowed `style` properties are fewer
//! still, and the email-safe output is post-processed anyway
//! (Outlook ignores most modern CSS). Pulling a 200KB DOM-based
//! sanitizer for ~80 lines of state-machine work is overkill.
//!
//! Threat model:
//! - **XSS injection**: `<script>`, `<iframe>`, `<object>`,
//!   `<embed>`, `on*` event handlers, `javascript:` URLs.
//!   All stripped.
//! - **CSS injection**: arbitrary `style="..."` values that
//!   the email client would render. Only a handful of safe
//!   properties pass; values are validated.
//! - **Tracking pixel injection**: `<img>` is NOT in the
//!   whitelist for inline rich text — Image blocks have their
//!   own typed shape. Authors who paste an image must use the
//!   Image block.
//!
//! Returns the sanitized HTML; non-allowed tags are dropped
//! AND their children are kept (`<script>foo</script>` →
//! `foo`). This preserves the operator's intent when they
//! accidentally type something that won't survive.

use std::borrow::Cow;

/// Tags allowed inline. Lowercase. Anything else is stripped
/// (children kept).
const ALLOWED_TAGS: &[&str] =
    &["strong", "b", "em", "i", "u", "br", "a", "span", "sub", "sup"];

/// Inline-style properties the renderer accepts. Email
/// clients honour the subset reliably; anything else gets
/// dropped.
const ALLOWED_STYLE_PROPS: &[&str] = &[
    "font-weight",
    "font-style",
    "text-decoration",
    "color",
    "background-color",
];

/// URL schemes safe to keep on `<a href>`. Anything else
/// becomes the empty string (link strips the href).
const ALLOWED_URL_SCHEMES: &[&str] = &["http://", "https://", "mailto:", "tel:"];

/// Sanitize a piece of operator-authored HTML. Returns owned
/// String even when no change is needed — caller doesn't need
/// the borrow distinction at this layer.
pub fn email_safe_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '<' {
            // Find end of tag.
            let rest = &input[i..];
            let close = rest.find('>').map(|p| i + p + 1);
            match close {
                Some(end) => {
                    let raw = &input[i..end];
                    let cleaned = clean_tag(raw);
                    out.push_str(&cleaned);
                    // Skip the chars we already consumed via `raw`.
                    while let Some(&(idx, _)) = chars.peek() {
                        if idx >= end {
                            break;
                        }
                        chars.next();
                    }
                }
                None => {
                    // Unterminated `<` — drop it.
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn clean_tag(raw: &str) -> Cow<'_, str> {
    // Strip surrounding < >
    let inner = &raw[1..raw.len() - 1];
    if inner.is_empty() {
        return Cow::Borrowed("");
    }
    let is_close = inner.starts_with('/');
    let body = if is_close { &inner[1..] } else { inner };
    let body = body.trim();
    let (tag_name_raw, rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
    let tag_name = tag_name_raw.trim_end_matches('/').to_ascii_lowercase();
    if !ALLOWED_TAGS.contains(&tag_name.as_str()) {
        // Drop the whole tag (children stay because we only
        // remove the markup).
        return Cow::Borrowed("");
    }
    if is_close {
        return Cow::Owned(format!("</{tag_name}>"));
    }
    // Self-closing void tag (`<br>`).
    let void = matches!(tag_name.as_str(), "br");
    let attrs = if rest.is_empty() {
        String::new()
    } else {
        clean_attrs(&tag_name, rest)
    };
    let close_marker = if void { " /" } else { "" };
    if attrs.is_empty() {
        Cow::Owned(format!("<{tag_name}{close_marker}>"))
    } else {
        Cow::Owned(format!("<{tag_name} {attrs}{close_marker}>"))
    }
}

fn clean_attrs(tag: &str, attrs_raw: &str) -> String {
    // Tiny attr parser: walks `name="value"` (or `name='value'`)
    // pairs. Quoted values only — bareword attrs are dropped
    // since email-safe HTML always uses quotes anyway.
    let mut out = Vec::new();
    let bytes = attrs_raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Read attr name.
        let name_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let name = attrs_raw[name_start..i].to_ascii_lowercase();
        if name.is_empty() {
            break;
        }
        // Expect `=`.
        if i >= bytes.len() || bytes[i] != b'=' {
            // Bareword attr (`disabled`). Skip — we don't
            // whitelist any bareword attr today.
            continue;
        }
        i += 1; // consume `=`
        // Expect quote.
        if i >= bytes.len() {
            break;
        }
        let quote = bytes[i];
        if quote != b'"' && quote != b'\'' {
            // Unquoted — skip the whole attr by walking to next
            // whitespace.
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }
        i += 1; // consume opening quote
        let val_start = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        let value = &attrs_raw[val_start..i];
        if i < bytes.len() {
            i += 1; // consume closing quote
        }
        // Drop `on*` handlers entirely.
        if name.starts_with("on") {
            continue;
        }
        if let Some(cleaned) = clean_one_attr(tag, &name, value) {
            out.push(cleaned);
        }
    }
    out.join(" ")
}

fn clean_one_attr(tag: &str, name: &str, value: &str) -> Option<String> {
    match name {
        "href" => {
            if tag != "a" {
                return None;
            }
            let scheme_ok = ALLOWED_URL_SCHEMES.iter().any(|s| {
                value.to_lowercase().starts_with(s)
            });
            if scheme_ok {
                Some(format!(
                    "href=\"{}\"",
                    escape_attr(value)
                ))
            } else {
                None
            }
        }
        "target" => {
            if tag != "a" {
                return None;
            }
            // Only `_blank` is meaningful in email; anything
            // else is harmless but noise.
            if value == "_blank" {
                Some("target=\"_blank\" rel=\"noopener\"".to_string())
            } else {
                None
            }
        }
        "style" => {
            let cleaned = clean_inline_style(value);
            if cleaned.is_empty() {
                None
            } else {
                Some(format!("style=\"{}\"", escape_attr(&cleaned)))
            }
        }
        // `title` is harmless on `<a>`.
        "title" if tag == "a" => Some(format!(
            "title=\"{}\"",
            escape_attr(value)
        )),
        // Drop everything else — class, id, data-*, srcset, etc.
        _ => None,
    }
}

fn clean_inline_style(value: &str) -> String {
    // Split by `;`, validate each `prop:value` pair against
    // ALLOWED_STYLE_PROPS. Drop anything with `expression(`,
    // `url(`, or `javascript:`.
    let mut keep = Vec::new();
    for decl in value.split(';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }
        let Some((prop, val)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        let val = val.trim();
        let val_lower = val.to_ascii_lowercase();
        if !ALLOWED_STYLE_PROPS.contains(&prop.as_str()) {
            continue;
        }
        if val_lower.contains("expression(")
            || val_lower.contains("url(")
            || val_lower.contains("javascript:")
            || val_lower.contains("@import")
        {
            continue;
        }
        // Sanity cap on value length to stop CSS bombs.
        if val.len() > 80 {
            continue;
        }
        keep.push(format!("{prop}: {val}"));
    }
    keep.join("; ")
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_strong_and_em() {
        let out = email_safe_html("<strong>bold</strong> and <em>italic</em>");
        assert_eq!(out, "<strong>bold</strong> and <em>italic</em>");
    }

    #[test]
    fn strips_script_tag() {
        let out = email_safe_html("hi <script>alert(1)</script> bye");
        // Tag stripped, text content preserved.
        assert_eq!(out, "hi alert(1) bye");
    }

    #[test]
    fn strips_event_handlers() {
        let out = email_safe_html(
            r#"<a href="https://x.com" onclick="evil()">link</a>"#
        );
        assert!(out.contains("href=\"https://x.com\""));
        assert!(!out.contains("onclick"));
    }

    #[test]
    fn rejects_javascript_href() {
        let out = email_safe_html(r#"<a href="javascript:alert(1)">link</a>"#);
        // href dropped → bare anchor with no link.
        assert!(out.contains("<a"));
        assert!(!out.contains("javascript"));
        assert!(!out.contains("href"));
    }

    #[test]
    fn allows_mailto_href() {
        let out = email_safe_html(r#"<a href="mailto:a@b.com">email</a>"#);
        assert!(out.contains("mailto:a@b.com"));
    }

    #[test]
    fn keeps_inline_style_whitelist() {
        let out = email_safe_html(
            r#"<span style="color: #ff0000; font-weight: bold">red</span>"#
        );
        assert!(out.contains("color: #ff0000"));
        assert!(out.contains("font-weight: bold"));
    }

    #[test]
    fn strips_unknown_style_props() {
        let out = email_safe_html(
            r#"<span style="position: fixed; color: red">x</span>"#
        );
        assert!(!out.contains("position"));
        assert!(out.contains("color: red"));
    }

    #[test]
    fn strips_css_url_in_style() {
        let out = email_safe_html(
            r#"<span style="background-color: url(http://evil.com)">x</span>"#
        );
        assert!(!out.contains("url("));
    }

    #[test]
    fn strips_class_attribute() {
        let out = email_safe_html(r#"<strong class="evil">hi</strong>"#);
        assert!(!out.contains("class"));
        assert!(out.contains("<strong>"));
    }

    #[test]
    fn br_self_closes() {
        let out = email_safe_html("line1<br>line2");
        assert!(out.contains("<br />") || out.contains("<br/>"));
    }

    #[test]
    fn drops_iframe() {
        let out = email_safe_html(
            r#"<iframe src="https://x.com">x</iframe>"#
        );
        // Tag dropped, child text preserved.
        assert!(!out.contains("iframe"));
        assert!(out.contains("x"));
    }

    #[test]
    fn unterminated_tag_drops_lt_keeps_text() {
        // The `<` itself is dropped (no matching `>`), but the
        // following text passes through. Acceptable behaviour —
        // the alternative (dropping the rest of the input) is
        // harsher than a forgotten `<`.
        let out = email_safe_html("hi <unterminated");
        assert_eq!(out, "hi unterminated");
    }

    #[test]
    fn target_blank_adds_noopener() {
        let out = email_safe_html(
            r#"<a href="https://x.com" target="_blank">x</a>"#
        );
        assert!(out.contains("target=\"_blank\""));
        assert!(out.contains("rel=\"noopener\""));
    }

    #[test]
    fn variable_placeholders_pass_through() {
        let out = email_safe_html("<strong>Hola {{name}}</strong>");
        assert_eq!(out, "<strong>Hola {{name}}</strong>");
    }

    #[test]
    fn nested_allowed_tags_preserved() {
        let out = email_safe_html("<strong><em>bold-italic</em></strong>");
        assert_eq!(out, "<strong><em>bold-italic</em></strong>");
    }
}
