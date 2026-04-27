//! Email-channel tool handlers (Phase 48.7).

pub mod archive;
pub mod bounces_summary;
pub mod context;
pub mod get;
pub mod imap_op;
pub mod label;
pub mod move_to;
pub mod reply;
pub mod search;
pub mod send;
pub mod thread;
pub mod uid_set;

#[cfg(test)]
pub mod dispatcher_stub;

pub use context::{DispatcherHandle, EmailToolContext};
pub use imap_op::{imap_date, imap_quote, run_imap_op};

use std::sync::Arc;

/// Stable list of every tool name this crate registers. Useful for
/// callers that want to pre-flight which patterns from
/// `agents.<id>.allowed_tools` actually match the email surface.
pub const EMAIL_TOOL_NAMES: &[&str] = &[
    "email_send",
    "email_reply",
    "email_archive",
    "email_move_to",
    "email_label",
    "email_search",
    "email_get",
    "email_thread",
    "email_bounces_summary",
];

/// Register every email-channel tool against the supplied registry.
/// Convenience for the no-filter case; equivalent to
/// `register_email_tools_filtered(registry, ctx, None)`.
pub fn register_email_tools(
    registry: &nexo_core::agent::tool_registry::ToolRegistry,
    ctx: Arc<EmailToolContext>,
) {
    register_email_tools_filtered(registry, ctx, None);
}

/// Phase 48 follow-up #9 — register only the email tools whose
/// names appear in `allow`. `None` registers every tool. An empty
/// slice is treated as "no email tools" (same as not calling
/// `register_email_tools` at all). Pattern matching mirrors the
/// agent-level `allowed_tools` semantics — exact name plus the
/// `email_*` glob is honoured by the caller before reaching here.
pub fn register_email_tools_filtered(
    registry: &nexo_core::agent::tool_registry::ToolRegistry,
    ctx: Arc<EmailToolContext>,
    allow: Option<&[String]>,
) {
    let want = |name: &str| -> bool {
        match allow {
            None => true,
            Some(list) => list.iter().any(|s| s == name),
        }
    };
    if want("email_send") {
        send::register(registry, ctx.clone());
    }
    if want("email_reply") {
        reply::register(registry, ctx.clone());
    }
    if want("email_archive") {
        archive::register(registry, ctx.clone());
    }
    if want("email_move_to") {
        move_to::register(registry, ctx.clone());
    }
    if want("email_label") {
        label::register(registry, ctx.clone());
    }
    if want("email_search") {
        search::register(registry, ctx.clone());
    }
    if want("email_get") {
        get::register(registry, ctx.clone());
    }
    if want("email_thread") {
        thread::register(registry, ctx.clone());
    }
    if want("email_bounces_summary") {
        bounces_summary::register(registry, ctx);
    }
}

/// Helper to derive an email-tool filter from an agent's
/// `allowed_tools` patterns. Glob `*` and `email_*` (or any
/// pattern containing `email`) preserves all six. Otherwise the
/// returned `Vec` lists the exact names that survived the pattern
/// match. An empty agent allowlist means "every tool" (current
/// agent semantics) so `None` is returned to preserve that.
pub fn filter_from_allowed_patterns(allowed_tools: &[String]) -> Option<Vec<String>> {
    if allowed_tools.is_empty() {
        return None;
    }
    let any_wildcard = allowed_tools.iter().any(|p| {
        p == "*" || p == "email_*" || p == "email*"
    });
    if any_wildcard {
        return None;
    }
    let kept: Vec<String> = EMAIL_TOOL_NAMES
        .iter()
        .filter(|name| allowed_tools.iter().any(|p| p == *name))
        .map(|s| s.to_string())
        .collect();
    Some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowed_returns_none() {
        assert!(filter_from_allowed_patterns(&[]).is_none());
    }

    #[test]
    fn star_wildcard_returns_none() {
        assert!(filter_from_allowed_patterns(&["*".into()]).is_none());
    }

    #[test]
    fn email_wildcard_returns_none() {
        assert!(filter_from_allowed_patterns(&["email_*".into()]).is_none());
        assert!(filter_from_allowed_patterns(&["email*".into()]).is_none());
    }

    #[test]
    fn explicit_names_filter_through() {
        let v = filter_from_allowed_patterns(&[
            "email_send".into(),
            "email_search".into(),
            "whatsapp_send".into(),
        ])
        .unwrap();
        assert_eq!(v, vec!["email_send".to_string(), "email_search".into()]);
    }

    #[test]
    fn unknown_email_name_drops_silently() {
        // Caller typo (`email_sennd`) — we don't register a fake
        // tool. Catalogue validation downstream surfaces the typo
        // separately.
        let v = filter_from_allowed_patterns(&["email_sennd".into()]).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn no_email_pattern_yields_empty_filter() {
        let v = filter_from_allowed_patterns(&[
            "whatsapp_send".into(),
            "telegram_send".into(),
        ])
        .unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn email_tool_names_includes_new_phase_48_tools() {
        for n in ["email_get", "email_thread", "email_bounces_summary"] {
            assert!(
                EMAIL_TOOL_NAMES.contains(&n),
                "expected EMAIL_TOOL_NAMES to contain {n}"
            );
        }
    }
}
