//! Strip lone UTF-16 surrogate code points before sending text to the
//! Anthropic API. Mirrors OpenClaw's
//! `sanitizeTransportPayloadText` (research/src/agents/transport-stream-shared.ts:22-27).
//!
//! Rust `&str` is guaranteed UTF-8, so surrogate halves (U+D800–U+DFFF)
//! cannot appear in well-formed input. The helper is a defensive guard
//! that mirrors the JS regex one-for-one and a zero-copy fast path for
//! the common case.

use std::borrow::Cow;

const SURROGATE_START: u32 = 0xD800;
const SURROGATE_END: u32 = 0xDFFF;

pub fn sanitize_payload_text(text: &str) -> Cow<'_, str> {
    if !contains_lone_surrogate(text) {
        return Cow::Borrowed(text);
    }
    let cleaned: String = text
        .chars()
        .filter(|c| {
            let code = *c as u32;
            !(SURROGATE_START..=SURROGATE_END).contains(&code)
        })
        .collect();
    Cow::Owned(cleaned)
}

fn contains_lone_surrogate(text: &str) -> bool {
    text.chars().any(|c| {
        let code = c as u32;
        (SURROGATE_START..=SURROGATE_END).contains(&code)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_valid_utf8_zero_copy() {
        let input = "hello world — Anthropic OAuth ✅";
        let out = sanitize_payload_text(input);
        assert!(matches!(out, Cow::Borrowed(_)), "expected zero-copy borrow");
        assert_eq!(out.as_ref(), input);
    }

    #[test]
    fn passes_through_valid_surrogate_pair_emoji() {
        // Emoji "🚀" encodes via a surrogate pair when interpreted as
        // UTF-16 but is a single Unicode scalar (U+1F680). UTF-8 in
        // Rust never exposes the surrogate halves, so the helper must
        // not strip it.
        let input = "rocket: 🚀";
        let out = sanitize_payload_text(input);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), input);
    }

    #[test]
    fn passes_through_empty_string() {
        let out = sanitize_payload_text("");
        assert_eq!(out.as_ref(), "");
        assert!(matches!(out, Cow::Borrowed(_)));
    }
}
