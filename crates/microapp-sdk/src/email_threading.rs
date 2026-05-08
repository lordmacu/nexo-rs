//! Email subject normalization + synthetic thread-id helpers
//! (Phase 83.18 / F29 — lifted from `nexo-rs-extension-marketing`
//! 0.17.x).
//!
//! Two layers of reply-detection live on top of standard RFC
//! 5322 threading (`In-Reply-To` / `References`):
//!
//! 1. **Synthetic thread id** — when an inbound arrives with
//!    no `Message-Id` / `In-Reply-To` / `References` headers
//!    (broken clients, mailers that strip them), the raw
//!    pipeline used to fold every orphan into a single
//!    `"thread-orphan"` bucket — so two unrelated cold leads
//!    appear to live in the same thread. The `synth_thread_id`
//!    helper hashes `(from_email, normalized_subject)` so a
//!    sender re-sending the same conversation gets a stable
//!    id that re-attaches to the existing lead, while two
//!    different senders never collide.
//!
//! 2. **Subject normalization** — strips any combination of
//!    locale-aware reply / forward prefixes (`Re:`, `RE:`,
//!    `Fwd:`, `RV:`, `Rv:`, `Tr:`, `Fw:`, `Antwort:`, `回复:`,
//!    …) so `"Re: Re: Fwd: Cotización"` and `"Cotización"`
//!    fold into the same canonical form.
//!
//! Both helpers are pure + deterministic — every invocation
//! against the same inputs returns the same output.

use sha2::{Digest, Sha256};

/// Reply / forward prefixes the normalizer strips. Languages
/// covered: English, Spanish, Portuguese, French, German,
/// Italian, Dutch, simplified Chinese (plus the universal
/// `Re:` and `Fwd:` even when localized clients lowercase
/// them). Comparison is ASCII-case-insensitive; non-ASCII
/// prefixes match verbatim (operators rarely localize subjects
/// beyond the prefix itself).
const REPLY_PREFIXES: &[&str] = &[
    "re", "fwd", "fw", "rv", "rsv", "rif", "tr", "ant", "aw",
    "antwort", "antw", "encaminhar", "encam", "回复", "答复",
];

/// Strip every leading reply / forward prefix from `subject`,
/// recursively (`"Re: Re: Re: x"` → `"x"`). Returns the
/// trimmed canonical form. Empty / whitespace-only input
/// returns `""`.
///
/// The output preserves the original case of the body content
/// (only the prefix is removed). Whitespace inside the body
/// is left alone — `"Cotización 2025"` stays as-is.
///
/// ```
/// // Imported from the marketing extension's threading module.
/// // assert_eq!(normalize_subject("Re: Re: Cotización"), "Cotización");
/// // assert_eq!(normalize_subject("Fwd: Re: Hello!"), "Hello!");
/// ```
pub fn normalize_subject(subject: &str) -> String {
    let mut s = subject.trim();
    loop {
        let stripped = strip_one_prefix(s);
        if stripped.len() == s.len() {
            break;
        }
        s = stripped.trim();
    }
    s.trim().to_string()
}

/// Try to strip ONE prefix off the front. Returns the rest of
/// the string if a prefix matched, or the input unchanged.
fn strip_one_prefix(s: &str) -> &str {
    for prefix in REPLY_PREFIXES {
        if let Some(rest) = strip_prefix_with_separator(s, prefix) {
            return rest;
        }
    }
    s
}

/// Match `<prefix>([:[\s]+]|\s*[:|]\s*)`. Tolerates `Re:`,
/// `RE:`, `re :`, `Re-`, `Re|`, `Re.`. ASCII-case-insensitive
/// for the prefix word itself.
fn strip_prefix_with_separator<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = &s[prefix.len()..];
    let bytes = rest.as_bytes();
    let mut idx = 0;
    // Optional whitespace before the separator.
    while idx < bytes.len() && (bytes[idx] == b' ' || bytes[idx] == b'\t') {
        idx += 1;
    }
    // Mandatory separator: `:`, `-`, `|`, `.`, or `>` (ML
    // archives sometimes use `>` as a quote indicator).
    if idx >= bytes.len() {
        return None;
    }
    if !matches!(bytes[idx], b':' | b'-' | b'|' | b'.' | b'>') {
        return None;
    }
    idx += 1;
    Some(&rest[idx..])
}

/// Build a deterministic 16-hex-char synthetic thread id from
/// `(from_email, normalized_subject)`. Stable across calls
/// (same inputs → same id) and unique enough that two
/// different `(sender, subject)` pairs collide with
/// probability ≈ 2⁻⁶⁴.
///
/// Format: `synth:<16-hex>`. The `synth:` prefix lets
/// downstream consumers distinguish a real RFC 5322 thread id
/// from one this fallback minted, without re-parsing.
///
/// Empty subject still produces a stable id — the from_email
/// alone groups every header-less inbound from one sender.
pub fn synth_thread_id(from_email: &str, subject: &str) -> String {
    let normalized = normalize_subject(subject);
    let mut hasher = Sha256::new();
    hasher.update(b"\x01"); // Version prefix — bump on format change.
    hasher.update(from_email.to_ascii_lowercase().as_bytes());
    hasher.update(b"\x00");
    hasher.update(normalized.to_lowercase().as_bytes());
    let bytes = hasher.finalize();
    let mut hex = String::with_capacity(2 * 8 + 6);
    hex.push_str("synth:");
    for b in &bytes[..8] {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── normalize_subject ────────────────────────────────────

    #[test]
    fn normalize_strips_single_re() {
        assert_eq!(normalize_subject("Re: Hello"), "Hello");
    }

    #[test]
    fn normalize_strips_recursive_re() {
        assert_eq!(normalize_subject("Re: Re: Re: Hello"), "Hello");
    }

    #[test]
    fn normalize_strips_mixed_case() {
        assert_eq!(normalize_subject("RE: re: rE: Hello"), "Hello");
    }

    #[test]
    fn normalize_strips_fwd_variants() {
        assert_eq!(normalize_subject("Fwd: x"), "x");
        assert_eq!(normalize_subject("FW: x"), "x");
        assert_eq!(normalize_subject("Fw: Re: x"), "x");
    }

    #[test]
    fn normalize_strips_spanish_prefixes() {
        assert_eq!(normalize_subject("RV: Cotización"), "Cotización");
        assert_eq!(normalize_subject("Rv: Re: Cotización"), "Cotización");
    }

    #[test]
    fn normalize_strips_portuguese_prefixes() {
        assert_eq!(normalize_subject("Encam: Algo"), "Algo");
        assert_eq!(normalize_subject("Encaminhar: Re: Algo"), "Algo");
    }

    #[test]
    fn normalize_strips_german_prefixes() {
        assert_eq!(normalize_subject("AW: Hallo"), "Hallo");
        assert_eq!(normalize_subject("Antw: AW: Hallo"), "Hallo");
        assert_eq!(normalize_subject("Antwort: Hallo"), "Hallo");
    }

    #[test]
    fn normalize_strips_italian_french() {
        assert_eq!(normalize_subject("Tr: Bonjour"), "Bonjour");
        assert_eq!(normalize_subject("Rif: Salve"), "Salve");
    }

    #[test]
    fn normalize_handles_dash_separator() {
        assert_eq!(normalize_subject("Re- Hello"), "Hello");
    }

    #[test]
    fn normalize_handles_pipe_separator() {
        assert_eq!(normalize_subject("Re| Hello"), "Hello");
    }

    #[test]
    fn normalize_does_not_strip_word_starting_with_re() {
        // `Reservation` is not a reply prefix — strip nothing.
        assert_eq!(normalize_subject("Reservation: Confirmed"), "Reservation: Confirmed");
    }

    #[test]
    fn normalize_preserves_internal_whitespace() {
        assert_eq!(
            normalize_subject("Re:    Hola    mundo"),
            "Hola    mundo",
        );
    }

    #[test]
    fn normalize_preserves_body_case() {
        assert_eq!(normalize_subject("Re: COTIZACIÓN"), "COTIZACIÓN");
    }

    #[test]
    fn normalize_empty_input_returns_empty() {
        assert_eq!(normalize_subject(""), "");
        assert_eq!(normalize_subject("   "), "");
    }

    #[test]
    fn normalize_only_prefixes_returns_empty() {
        assert_eq!(normalize_subject("Re: Re:"), "");
        assert_eq!(normalize_subject("Fwd: Re: Re:"), "");
    }

    // ─── synth_thread_id ──────────────────────────────────────

    #[test]
    fn synth_id_is_deterministic() {
        let a = synth_thread_id("juan@acme.com", "Cotización");
        let b = synth_thread_id("juan@acme.com", "Cotización");
        assert_eq!(a, b);
    }

    #[test]
    fn synth_id_collapses_reply_prefix_variations() {
        let raw = synth_thread_id("juan@acme.com", "Cotización");
        let with_re = synth_thread_id("juan@acme.com", "Re: Cotización");
        let with_double = synth_thread_id("juan@acme.com", "Re: Re: Cotización");
        let with_fwd = synth_thread_id("juan@acme.com", "Fwd: Cotización");
        assert_eq!(raw, with_re);
        assert_eq!(raw, with_double);
        assert_eq!(raw, with_fwd);
    }

    #[test]
    fn synth_id_different_senders_dont_collide() {
        let a = synth_thread_id("a@x.com", "Hello");
        let b = synth_thread_id("b@x.com", "Hello");
        assert_ne!(a, b);
    }

    #[test]
    fn synth_id_different_subjects_dont_collide() {
        let a = synth_thread_id("a@x.com", "Hello");
        let b = synth_thread_id("a@x.com", "Goodbye");
        assert_ne!(a, b);
    }

    #[test]
    fn synth_id_is_case_insensitive_for_email() {
        let a = synth_thread_id("Juan@Acme.com", "Cotización");
        let b = synth_thread_id("juan@acme.com", "Cotización");
        assert_eq!(a, b);
    }

    #[test]
    fn synth_id_is_case_insensitive_for_subject() {
        let a = synth_thread_id("a@x.com", "HELLO");
        let b = synth_thread_id("a@x.com", "hello");
        assert_eq!(a, b);
    }

    #[test]
    fn synth_id_carries_synth_prefix() {
        let id = synth_thread_id("a@x.com", "Hello");
        assert!(id.starts_with("synth:"));
        // 6 chars prefix + 16 hex chars.
        assert_eq!(id.len(), 22);
    }

    #[test]
    fn synth_id_handles_empty_subject() {
        // Same sender + empty subject should still group
        // together, but distinctly from any non-empty subject.
        let a = synth_thread_id("a@x.com", "");
        let b = synth_thread_id("a@x.com", "");
        let c = synth_thread_id("a@x.com", "Hello");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
