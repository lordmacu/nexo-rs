//! CSS / URL sanitisation primitives.
//!
//! These two helpers guard against injection in operator-
//! supplied style fields. They were originally written for the
//! marketing extension's email-template renderer (per-row /
//! per-column / per-page backgrounds where an operator picks a
//! colour and pastes an image URL); the logic itself has
//! nothing to do with email and any microapp that renders
//! operator-authored CSS needs the same guardrails.
//!
//! Both functions are pure-Rust string validators — no regex,
//! no heap dependencies, no allocations on the happy path that
//! aren't already in the input. They're safe to call on every
//! render.

/// Validate a CSS colour value. Accepts:
/// - `#rgb` (3 hex chars after `#`)
/// - `#rrggbb` (6 hex chars after `#`)
/// - `#rrggbbaa` (8 hex chars after `#`)
/// - a small named palette: transparent, white, black, red,
///   green, blue, yellow, orange, purple, gray, grey, silver,
///   teal, navy
///
/// Anything else falls back to `default` so a malicious template
/// can't inject CSS via the colour field.
///
/// Case-insensitive on the named palette; hex chars must be
/// `0-9a-fA-F`.
pub fn sanitize_color(input: &str, default: &str) -> String {
    let s = input.trim();
    if s.starts_with('#') && s.chars().skip(1).all(|c| c.is_ascii_hexdigit()) {
        let len = s.len() - 1;
        if len == 3 || len == 6 || len == 8 {
            return s.to_string();
        }
    }
    // Allow a tiny named palette commonly used in operator
    // emails / dashboards. Avoids dragging in a full CSS color
    // crate for a 14-name list.
    const NAMED: &[&str] = &[
        "transparent",
        "white",
        "black",
        "red",
        "green",
        "blue",
        "yellow",
        "orange",
        "purple",
        "gray",
        "grey",
        "silver",
        "teal",
        "navy",
    ];
    if NAMED.contains(&s.to_lowercase().as_str()) {
        return s.to_string();
    }
    default.to_string()
}

/// Validate a background-image / asset URL. Allowlists `http://`
/// and `https://` schemes only — `javascript:`, `data:`,
/// `file:`, scheme-relative `//host`, and bare relative paths
/// all rejected. Also rejects any character that would break
/// out of a CSS `url(...)` wrapper or an HTML
/// `background="..."` attribute (quotes, parens, whitespace,
/// control bytes, angle brackets, backslash).
///
/// Returns `None` on rejection so the caller can drop the
/// property entirely (a half-rendered `url()` would inherit the
/// surrounding colour silently — better to skip the rule).
///
/// Length capped at 2048 bytes — way more than any realistic
/// CDN URL, but defends against pathological inputs that bloat
/// the rendered HTML.
pub fn sanitize_url(input: &str) -> Option<String> {
    let s = input.trim();
    if !(s.starts_with("http://") || s.starts_with("https://")) {
        return None;
    }
    if s.len() > 2048 {
        return None;
    }
    for c in s.chars() {
        if c.is_whitespace() || c.is_control() {
            return None;
        }
        if matches!(c, '"' | '\'' | '(' | ')' | '<' | '>' | '\\') {
            return None;
        }
    }
    Some(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sanitize_color ────────────────────────────────────────

    #[test]
    fn color_accepts_rgb_short() {
        assert_eq!(sanitize_color("#abc", "#000"), "#abc");
    }

    #[test]
    fn color_accepts_rrggbb() {
        assert_eq!(sanitize_color("#aabbcc", "#000"), "#aabbcc");
    }

    #[test]
    fn color_accepts_rrggbbaa() {
        assert_eq!(sanitize_color("#aabbccdd", "#000"), "#aabbccdd");
    }

    #[test]
    fn color_accepts_uppercase_hex() {
        assert_eq!(sanitize_color("#FFAA00", "#000"), "#FFAA00");
    }

    #[test]
    fn color_accepts_named_palette() {
        for name in &[
            "transparent",
            "white",
            "black",
            "red",
            "green",
            "blue",
            "yellow",
            "orange",
            "purple",
            "gray",
            "grey",
            "silver",
            "teal",
            "navy",
        ] {
            assert_eq!(sanitize_color(name, "#000"), *name, "name {name}");
        }
    }

    #[test]
    fn color_named_is_case_insensitive() {
        assert_eq!(sanitize_color("WHITE", "#000"), "WHITE");
        assert_eq!(sanitize_color("Red", "#000"), "Red");
    }

    #[test]
    fn color_rejects_css_injection() {
        // Classic CSS expression / IE expression() / url() with
        // semicolons + closing brace to break out of the style
        // attribute.
        for bad in &[
            "expression(alert(1))",
            "red; background:url(x)",
            "#fff; behavior:url(x)",
            "javascript:1",
            "rgb(255,0,0)", // no rgb() form for now
            "#zzz",         // non-hex chars
            "#1234",        // 4 chars not allowed
            "#1234567",     // 7 chars not allowed
            "",
            "  ",
        ] {
            assert_eq!(
                sanitize_color(bad, "#default"),
                "#default",
                "should reject: {bad:?}"
            );
        }
    }

    #[test]
    fn color_trims_whitespace() {
        assert_eq!(sanitize_color("  #abc  ", "#000"), "#abc");
    }

    // ── sanitize_url ──────────────────────────────────────────

    #[test]
    fn url_accepts_https() {
        assert_eq!(
            sanitize_url("https://cdn.example.com/bg.jpg"),
            Some("https://cdn.example.com/bg.jpg".to_string())
        );
    }

    #[test]
    fn url_accepts_http() {
        assert_eq!(
            sanitize_url("http://x.test/i.png"),
            Some("http://x.test/i.png".to_string())
        );
    }

    #[test]
    fn url_rejects_dangerous_schemes() {
        for bad in &[
            "javascript:alert(1)",
            "data:image/png;base64,AAA",
            "file:///etc/passwd",
            "//evil.com/x.png",
            "/relative.png",
            "ftp://x.test/a.png",
            "",
        ] {
            assert!(sanitize_url(bad).is_none(), "should reject: {bad:?}");
        }
    }

    #[test]
    fn url_rejects_css_breakers() {
        for bad in &[
            "https://x.test/a)b.png",
            "https://x.test/a(b.png",
            r#"https://x.test/a"b.png"#,
            "https://x.test/a'b.png",
            "https://x.test/a b.png",
            "https://x.test/a<b.png",
            "https://x.test/a>b.png",
            "https://x.test/a\\b.png",
            "https://x.test/a\nb.png",
            "https://x.test/a\tb.png",
        ] {
            assert!(sanitize_url(bad).is_none(), "should reject: {bad:?}");
        }
    }

    #[test]
    fn url_caps_length() {
        let long = format!("https://x.test/{}", "a".repeat(2048));
        assert!(sanitize_url(&long).is_none());
    }

    #[test]
    fn url_trims_whitespace() {
        assert_eq!(
            sanitize_url("  https://x.test/a.png  "),
            Some("https://x.test/a.png".to_string())
        );
    }
}
