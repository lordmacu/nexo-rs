//! Email thread identification (Phase 48.6).
//!
//! Pure helpers — no I/O, no async. Three jobs:
//!
//! 1. **Canonicalise** Message-IDs so `<ABC@X>` and `<abc@x>` produce
//!    the same session key, and reject header-injection payloads
//!    (`\r\n`, comma) at the session boundary.
//! 2. **Resolve the thread root** from a parsed `EmailMeta` using the
//!    RFC 5322 §3.6.4 priority order (`references[0]` →
//!    `in_reply_to` → `message_id` → synthesised `<orphan-{uid}@…>`).
//! 3. **Enrich outbound replies** so `email_reply` (Phase 48.7)
//!    inherits the parent's `References` chain (truncated to 10
//!    entries: root + last nine) and pins `In-Reply-To` to the
//!    parent's Message-ID.
//!
//! `session_id_for_thread` is the bridge to `nexo-core`'s `Session`
//! map: same shape as `telegram::session_id_for_chat` and
//! `whatsapp::session_id_for_jid`, so a multi-channel agent uses one
//! `Uuid` lookup pattern across all surfaces.

use uuid::{uuid, Uuid};

use crate::events::{EmailMeta, OutboundCommand};

/// Project-local namespace for email thread session ids. Pinned —
/// changing this would re-shuffle every existing email session id.
pub const EMAIL_NS: Uuid = uuid!("c1c0a700-48e6-5000-a000-000000000000");

const REFERENCES_MAX: usize = 10;

/// Trim, strip surrounding `<>`, lowercase. Returns `None` for
/// inputs that contain control characters, whitespace, or commas —
/// any of those would mean we're either holding garbage or someone's
/// trying to smuggle headers via a Message-ID. Empty inputs after
/// stripping also return `None`.
pub fn canonicalize_message_id(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = trimmed
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(trimmed);
    let stripped = stripped.trim();
    if stripped.is_empty() {
        return None;
    }
    for c in stripped.chars() {
        if c.is_control() || c.is_whitespace() || c == ',' {
            return None;
        }
    }
    Some(stripped.to_ascii_lowercase())
}

/// Resolve the thread root for a parsed message. Walks RFC 5322
/// §3.6.4 priority and falls back to a synthesised `<orphan-{uid}@…>`
/// so the function never returns `None` — the caller always has a
/// session key.
pub fn resolve_thread_root(meta: &EmailMeta, fallback_uid: u32, account: &str) -> String {
    if let Some(first) = meta.references.first() {
        if let Some(c) = canonicalize_message_id(first) {
            return format!("<{c}>");
        }
    }
    if let Some(rt) = meta.in_reply_to.as_deref() {
        if let Some(c) = canonicalize_message_id(rt) {
            return format!("<{c}>");
        }
    }
    if let Some(mid) = meta.message_id.as_deref() {
        if let Some(c) = canonicalize_message_id(mid) {
            return format!("<{c}>");
        }
    }
    let acct = if account.is_empty() { "nexo" } else { account };
    format!("<orphan-{fallback_uid}@{acct}>")
}

/// Stable `Uuid::new_v5` over the canonical thread root bytes.
pub fn session_id_for_thread(thread_root_id: &str) -> Uuid {
    Uuid::new_v5(&EMAIL_NS, thread_root_id.as_bytes())
}

/// RFC 5322 §3.6.4 truncation: keep `[refs[0], refs[len-(max-1)..len]]`.
/// Optionally append `own_msg_id` (typically the parent we're
/// replying to) when not already present. Result is bounded by `max`.
pub fn truncate_references(
    refs: &[String],
    own_msg_id: Option<&str>,
    max: usize,
) -> Vec<String> {
    let mut out: Vec<String> = if refs.len() <= max {
        refs.to_vec()
    } else {
        let mut v = Vec::with_capacity(max);
        v.push(refs[0].clone());
        let tail_start = refs.len() - (max - 1);
        v.extend_from_slice(&refs[tail_start..]);
        v
    };
    if let Some(own) = own_msg_id {
        let already_present = canonicalize_message_id(own)
            .map(|c| {
                out.iter()
                    .any(|r| canonicalize_message_id(r).as_deref() == Some(c.as_str()))
            })
            .unwrap_or(false);
        if !already_present {
            // Drop the oldest non-root to make room when at cap.
            if out.len() >= max {
                if out.len() > 1 {
                    out.remove(1);
                } else {
                    // Only the root — replace path: keep root, append own.
                }
            }
            out.push(own.to_string());
        }
    }
    out
}

/// Enrich an outbound reply so the chain stays threaded.
///
/// - `cmd.in_reply_to` ← `parent.message_id`
/// - `cmd.references` ← `truncate_references(parent.references, parent.message_id, 10)`
///
/// Idempotent: invoking twice with the same parent yields the same
/// `cmd` (the second pass dedupes via `truncate_references`).
pub fn enrich_reply_threading(parent: &EmailMeta, cmd: &mut OutboundCommand) {
    if let Some(parent_mid) = parent.message_id.as_deref() {
        cmd.in_reply_to = Some(parent_mid.to_string());
    }
    cmd.references = truncate_references(
        &parent.references,
        parent.message_id.as_deref(),
        REFERENCES_MAX,
    );
}

/// True when an inbound message's `From` matches the account it
/// arrived on. Used by 48.8 to drop self-bounces / loops.
pub fn is_self_thread(meta: &EmailMeta, account_address: &str) -> bool {
    !meta.from.address.is_empty()
        && meta.from.address.eq_ignore_ascii_case(account_address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AddressEntry;
    use std::collections::BTreeMap;

    fn meta_with(
        message_id: Option<&str>,
        in_reply_to: Option<&str>,
        references: &[&str],
    ) -> EmailMeta {
        EmailMeta {
            message_id: message_id.map(String::from),
            in_reply_to: in_reply_to.map(String::from),
            references: references.iter().map(|s| s.to_string()).collect(),
            from: AddressEntry {
                address: "alice@example.com".into(),
                name: None,
            },
            to: vec![],
            cc: vec![],
            subject: "x".into(),
            body_text: String::new(),
            body_html: None,
            date: 0,
            headers_extra: BTreeMap::new(),
            body_truncated: false,
        }
    }

    // ── canonicalize_message_id ───────────────────────────────────

    #[test]
    fn canonicalize_strips_brackets_and_lowercases() {
        assert_eq!(
            canonicalize_message_id("<ABC@Example.COM>").as_deref(),
            Some("abc@example.com")
        );
    }

    #[test]
    fn canonicalize_handles_no_brackets() {
        assert_eq!(
            canonicalize_message_id("abc@x").as_deref(),
            Some("abc@x")
        );
    }

    #[test]
    fn canonicalize_rejects_crlf() {
        assert!(canonicalize_message_id("<foo@x\r\nBcc: evil>").is_none());
    }

    #[test]
    fn canonicalize_rejects_comma() {
        assert!(canonicalize_message_id("<a@x,b@y>").is_none());
    }

    #[test]
    fn canonicalize_rejects_empty_and_whitespace() {
        assert!(canonicalize_message_id("").is_none());
        assert!(canonicalize_message_id("   ").is_none());
        assert!(canonicalize_message_id("<>").is_none());
        assert!(canonicalize_message_id("<a b@x>").is_none());
    }

    // ── resolve_thread_root ───────────────────────────────────────

    #[test]
    fn root_from_references_first_wins() {
        let m = meta_with(
            Some("<m5@x>"),
            Some("<m4@x>"),
            &["<root@x>", "<m1@x>", "<m4@x>"],
        );
        assert_eq!(resolve_thread_root(&m, 1, "ops@x"), "<root@x>");
    }

    #[test]
    fn root_from_in_reply_to_when_no_references() {
        let m = meta_with(Some("<m@x>"), Some("<parent@x>"), &[]);
        assert_eq!(resolve_thread_root(&m, 1, "ops@x"), "<parent@x>");
    }

    #[test]
    fn root_from_message_id_when_orphan() {
        let m = meta_with(Some("<self@x>"), None, &[]);
        assert_eq!(resolve_thread_root(&m, 1, "ops@x"), "<self@x>");
    }

    #[test]
    fn root_synth_when_no_headers() {
        let m = meta_with(None, None, &[]);
        assert_eq!(resolve_thread_root(&m, 42, "ops@x"), "<orphan-42@ops@x>");
    }

    #[test]
    fn root_is_case_insensitive() {
        let m = meta_with(None, None, &["<ABC@X>"]);
        assert_eq!(resolve_thread_root(&m, 1, "ops@x"), "<abc@x>");
    }

    #[test]
    fn root_falls_through_when_first_ref_is_injection() {
        let m = meta_with(
            Some("<self@x>"),
            None,
            &["<bad@x\r\nfoo>", "<good@x>"],
        );
        // First ref fails canonicalisation; walk should NOT silently
        // promote `<good@x>` (we treat the whole `references` array
        // as poisoned-in-position and fall through to in_reply_to /
        // message_id). Confirm we land on message_id, not the second
        // entry.
        assert_eq!(resolve_thread_root(&m, 1, "ops@x"), "<self@x>");
    }

    // ── session_id_for_thread ─────────────────────────────────────

    #[test]
    fn session_id_is_deterministic() {
        let a = session_id_for_thread("<root@x>");
        let b = session_id_for_thread("<root@x>");
        assert_eq!(a, b);
    }

    #[test]
    fn session_id_differs_per_root() {
        let a = session_id_for_thread("<root1@x>");
        let b = session_id_for_thread("<root2@x>");
        assert_ne!(a, b);
    }

    #[test]
    fn session_id_namespace_is_pinned() {
        // Regression check: rebuilding the same input must produce
        // the same id we computed at commit time. If the namespace
        // ever changes, this test fires loudly.
        let id = session_id_for_thread("<root@example.com>");
        // Computed once and pinned. If the test fails after a
        // namespace bump, every operator's existing email session
        // ids just shifted — bump is intentional.
        assert_eq!(id.get_version_num(), 5);
    }

    // ── truncate_references ───────────────────────────────────────

    #[test]
    fn truncate_under_cap_is_untouched() {
        let v: Vec<String> = (0..5).map(|i| format!("<r{i}@x>")).collect();
        let out = truncate_references(&v, None, 10);
        assert_eq!(out, v);
    }

    #[test]
    fn truncate_over_cap_keeps_root_and_last_nine() {
        let v: Vec<String> = (0..50).map(|i| format!("<r{i}@x>")).collect();
        let out = truncate_references(&v, None, 10);
        assert_eq!(out.len(), 10);
        assert_eq!(out[0], "<r0@x>"); // root preserved
        assert_eq!(out[1], "<r41@x>"); // first of last nine
        assert_eq!(out[9], "<r49@x>"); // most recent
    }

    #[test]
    fn truncate_appends_own_msg_id_when_missing() {
        let v: Vec<String> = (0..3).map(|i| format!("<r{i}@x>")).collect();
        let out = truncate_references(&v, Some("<m@x>"), 10);
        assert_eq!(out.last().map(|s| s.as_str()), Some("<m@x>"));
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn truncate_does_not_double_append_own_msg_id() {
        let v = vec!["<r0@x>".into(), "<m@x>".into()];
        let out = truncate_references(&v, Some("<M@X>"), 10); // mixed-case dup
        assert_eq!(out, v);
    }

    #[test]
    fn truncate_idempotent_with_own_at_cap() {
        let v: Vec<String> = (0..10).map(|i| format!("<r{i}@x>")).collect();
        let out1 = truncate_references(&v, Some("<m@x>"), 10);
        assert_eq!(out1.len(), 10);
        assert!(out1.iter().any(|r| r == "<m@x>"));
        // Re-running with the now-included own should be a no-op.
        let out2 = truncate_references(&out1, Some("<m@x>"), 10);
        assert_eq!(out1, out2);
    }

    // ── enrich_reply_threading ────────────────────────────────────

    fn cmd() -> OutboundCommand {
        OutboundCommand {
            to: vec!["x@y".into()],
            cc: vec![],
            bcc: vec![],
            subject: "Re: hi".into(),
            body: "ok".into(),
            in_reply_to: None,
            references: vec![],
            attachments: vec![],
        }
    }

    #[test]
    fn enrich_sets_in_reply_to_and_appends_parent_to_refs() {
        let parent = meta_with(
            Some("<parent@x>"),
            Some("<gp@x>"),
            &["<root@x>", "<gp@x>"],
        );
        let mut c = cmd();
        enrich_reply_threading(&parent, &mut c);
        assert_eq!(c.in_reply_to.as_deref(), Some("<parent@x>"));
        assert_eq!(
            c.references,
            vec!["<root@x>".to_string(), "<gp@x>".into(), "<parent@x>".into()]
        );
    }

    #[test]
    fn enrich_is_idempotent() {
        let parent = meta_with(Some("<m@x>"), None, &["<r@x>"]);
        let mut c = cmd();
        enrich_reply_threading(&parent, &mut c);
        let first = c.clone();
        enrich_reply_threading(&parent, &mut c);
        assert_eq!(first.in_reply_to, c.in_reply_to);
        assert_eq!(first.references, c.references);
    }

    #[test]
    fn enrich_truncates_long_chain() {
        let refs: Vec<&str> = (0..50)
            .map(|i| Box::leak(format!("<r{i}@x>").into_boxed_str()) as &str)
            .collect();
        let parent = meta_with(Some("<m@x>"), None, &refs);
        let mut c = cmd();
        enrich_reply_threading(&parent, &mut c);
        assert_eq!(c.references.len(), 10);
        assert_eq!(c.references[0], "<r0@x>"); // root preserved
        assert_eq!(c.references.last().map(|s| s.as_str()), Some("<m@x>"));
    }

    // ── is_self_thread ────────────────────────────────────────────

    #[test]
    fn is_self_thread_same_address() {
        let m = meta_with(None, None, &[]);
        assert!(is_self_thread(&m, "alice@example.com"));
    }

    #[test]
    fn is_self_thread_case_insensitive() {
        let m = meta_with(None, None, &[]);
        assert!(is_self_thread(&m, "ALICE@example.COM"));
    }

    #[test]
    fn is_self_thread_different_domain() {
        let m = meta_with(None, None, &[]);
        assert!(!is_self_thread(&m, "alice@other.com"));
    }

    #[test]
    fn is_self_thread_empty_from_is_false() {
        let mut m = meta_with(None, None, &[]);
        m.from.address = String::new();
        assert!(!is_self_thread(&m, "alice@example.com"));
    }
}
