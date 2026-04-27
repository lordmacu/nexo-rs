//! Loop-prevention helpers (Phase 48.8).
//!
//! Pure decision function the worker calls between parse and
//! publish to drop messages that would otherwise re-trigger the
//! agent in a loop. Reasons are surfaced as `SkipReason` so the
//! worker can log + count them by category.
//!
//! Order matters — first match wins:
//! 1. `Auto-Submitted` (RFC 3834) — explicit auto-reply / OOO bot
//! 2. `List-Id` / `List-Unsubscribe` (RFC 2369) — mailing list
//! 3. `Precedence: bulk|junk|list` (RFC 2076) — non-list bulk
//! 4. `is_self_thread` — bounce-back of our own outbound
//!
//! The DSN path (`Phase 48.8 dsn.rs::parse_bounce`) runs *before*
//! `should_skip` in `drain_pending` so a delivery report still
//! emits a `BounceEvent` even when the report itself happens to
//! ship `Auto-Submitted` (most do).

use nexo_config::types::plugins::LoopPreventionCfg;

use crate::events::EmailMeta;
use crate::threading::is_self_thread;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    AutoSubmitted,
    ListMail,
    PrecedenceBulk,
    SelfFrom,
    /// Set by `drain_pending` after `parse_bounce` returned `Some`.
    /// Never produced by `should_skip` itself.
    DsnInbound,
}

impl SkipReason {
    pub const fn metric_label(&self) -> &'static str {
        match self {
            Self::AutoSubmitted => "auto_submitted",
            Self::ListMail => "list_mail",
            Self::PrecedenceBulk => "precedence_bulk",
            Self::SelfFrom => "self_from",
            Self::DsnInbound => "dsn_inbound",
        }
    }
}

/// Decide whether the worker should skip publishing this inbound.
/// Returns `None` for messages that should flow normally to the
/// agent.
pub fn should_skip(
    meta: &EmailMeta,
    account_address: &str,
    cfg: &LoopPreventionCfg,
) -> Option<SkipReason> {
    if cfg.auto_submitted {
        if let Some(v) = meta.headers_extra.get("auto-submitted") {
            // RFC 3834: `no` is the explicit "this is a regular
            // human-authored message" marker. Anything else
            // (`auto-replied`, `auto-generated`, `auto-notified`)
            // is the loop signal.
            if !v.trim().eq_ignore_ascii_case("no") {
                return Some(SkipReason::AutoSubmitted);
            }
        }
    }

    if cfg.list_headers {
        if meta.headers_extra.contains_key("list-id")
            || meta.headers_extra.contains_key("list-unsubscribe")
        {
            return Some(SkipReason::ListMail);
        }
        if let Some(prec) = meta.headers_extra.get("precedence") {
            let p = prec.trim().to_ascii_lowercase();
            if p == "bulk" || p == "junk" || p == "list" {
                return Some(SkipReason::PrecedenceBulk);
            }
        }
    }

    if cfg.self_from && is_self_thread(meta, account_address) {
        return Some(SkipReason::SelfFrom);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AddressEntry;
    use std::collections::BTreeMap;

    fn meta_from(from: &str) -> EmailMeta {
        EmailMeta {
            message_id: None,
            in_reply_to: None,
            references: vec![],
            from: AddressEntry {
                address: from.into(),
                name: None,
            },
            to: vec![],
            cc: vec![],
            subject: String::new(),
            body_text: String::new(),
            body_html: None,
            date: 0,
            headers_extra: BTreeMap::new(),
            body_truncated: false,
        }
    }

    fn cfg_all() -> LoopPreventionCfg {
        LoopPreventionCfg::default()
    }

    #[test]
    fn auto_submitted_auto_replied_skips() {
        let mut m = meta_from("alice@x");
        m.headers_extra
            .insert("auto-submitted".into(), "auto-replied".into());
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), Some(SkipReason::AutoSubmitted));
    }

    #[test]
    fn auto_submitted_no_does_not_skip() {
        let mut m = meta_from("alice@x");
        m.headers_extra.insert("auto-submitted".into(), "no".into());
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), None);
    }

    #[test]
    fn list_id_skips_as_list_mail() {
        let mut m = meta_from("bot@list.x");
        m.headers_extra
            .insert("list-id".into(), "<weekly.list.x>".into());
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), Some(SkipReason::ListMail));
    }

    #[test]
    fn list_unsubscribe_skips_as_list_mail() {
        let mut m = meta_from("bot@list.x");
        m.headers_extra
            .insert("list-unsubscribe".into(), "<https://list.x/u>".into());
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), Some(SkipReason::ListMail));
    }

    #[test]
    fn precedence_bulk_skips() {
        let mut m = meta_from("bot@x");
        m.headers_extra.insert("precedence".into(), "bulk".into());
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), Some(SkipReason::PrecedenceBulk));
    }

    #[test]
    fn precedence_junk_skips_case_insensitive() {
        let mut m = meta_from("bot@x");
        m.headers_extra.insert("precedence".into(), "JUNK".into());
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), Some(SkipReason::PrecedenceBulk));
    }

    #[test]
    fn self_from_skips() {
        let m = meta_from("ops@x");
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), Some(SkipReason::SelfFrom));
    }

    #[test]
    fn cfg_off_skips_nothing() {
        let mut m = meta_from("ops@x");
        m.headers_extra
            .insert("auto-submitted".into(), "auto-replied".into());
        m.headers_extra.insert("list-id".into(), "<l@x>".into());
        let cfg = LoopPreventionCfg {
            auto_submitted: false,
            list_headers: false,
            self_from: false,
        };
        assert_eq!(should_skip(&m, "ops@x", &cfg), None);
    }

    #[test]
    fn auto_submitted_wins_over_list_id() {
        // Both signals present; auto_submitted is checked first.
        let mut m = meta_from("alice@x");
        m.headers_extra
            .insert("auto-submitted".into(), "auto-replied".into());
        m.headers_extra.insert("list-id".into(), "<l@x>".into());
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), Some(SkipReason::AutoSubmitted));
    }
}
