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
//! 4. `X-Spam-Flag: YES` — upstream spam filter verdict
//! 5. `Feedback-ID` — ESP mass-mail tracking (RFC 6438)
//! 6. `X-Mailer` matches known ESP — mass-mail provider signature
//! 7. `is_self_thread` — bounce-back of our own outbound
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
    /// `X-Spam-Flag: YES` (upstream filter such as SpamAssassin
    /// / Rspamd already classified the message as spam).
    SpamFlag,
    /// `Feedback-ID` header present — ESP mass-mail tracking
    /// (RFC 6438 / Google FBL convention).
    FeedbackId,
    /// `X-Mailer` matches a known ESP signature (Mailchimp,
    /// SendGrid, Mailgun, Marketo, Constant Contact, …).
    EspMailer,
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
            Self::SpamFlag => "spam_flag",
            Self::FeedbackId => "feedback_id",
            Self::EspMailer => "esp_mailer",
            Self::DsnInbound => "dsn_inbound",
        }
    }
}

/// Substring signatures that flag the `X-Mailer` / `User-Agent`
/// header as a known mass-mail ESP. Lowercase comparison; any
/// hit drops the message. Conservative list — only providers
/// whose presence implies bulk by definition (the same providers
/// also power transactional mail, but transactional mail almost
/// always also ships `Feedback-ID` or `List-Unsubscribe` so it's
/// already covered by the earlier rules).
const ESP_MAILER_SIGNATURES: &[&str] = &[
    "mailchimp",
    "sendgrid",
    "mailgun",
    "marketo",
    "constant contact",
    "constantcontact",
    "sendinblue",
    "brevo",
    "campaign monitor",
    "campaignmonitor",
    "klaviyo",
    "hubspot",
    "amazonses",
    "amazon ses",
    "mandrill",
    "postmark",
    "elasticemail",
    "elastic email",
    "getresponse",
    "activecampaign",
    "doppleremailer",
    "mailerlite",
    "mailjet",
    "convertkit",
    "drip",
    "omnisend",
    "moosend",
];

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

    if cfg.spam_flag {
        if let Some(v) = meta.headers_extra.get("x-spam-flag") {
            // Convention: `YES` (any case) means the upstream
            // filter classified as spam. Some servers emit
            // `True` / `Y`; treat any non-`no`/`false` value as
            // a spam verdict to be safe.
            let v = v.trim().to_ascii_lowercase();
            if v == "yes" || v == "true" || v == "y" {
                return Some(SkipReason::SpamFlag);
            }
        }
    }

    if cfg.feedback_id && meta.headers_extra.contains_key("feedback-id") {
        return Some(SkipReason::FeedbackId);
    }

    if cfg.esp_mailer {
        if let Some(mailer) = meta.headers_extra.get("x-mailer") {
            let m = mailer.to_ascii_lowercase();
            if ESP_MAILER_SIGNATURES.iter().any(|sig| m.contains(sig)) {
                return Some(SkipReason::EspMailer);
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
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::AutoSubmitted)
        );
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
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::ListMail)
        );
    }

    #[test]
    fn list_unsubscribe_skips_as_list_mail() {
        let mut m = meta_from("bot@list.x");
        m.headers_extra
            .insert("list-unsubscribe".into(), "<https://list.x/u>".into());
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::ListMail)
        );
    }

    #[test]
    fn precedence_bulk_skips() {
        let mut m = meta_from("bot@x");
        m.headers_extra.insert("precedence".into(), "bulk".into());
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::PrecedenceBulk)
        );
    }

    #[test]
    fn precedence_junk_skips_case_insensitive() {
        let mut m = meta_from("bot@x");
        m.headers_extra.insert("precedence".into(), "JUNK".into());
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::PrecedenceBulk)
        );
    }

    #[test]
    fn self_from_skips() {
        let m = meta_from("ops@x");
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::SelfFrom)
        );
    }

    #[test]
    fn cfg_off_skips_nothing() {
        let mut m = meta_from("ops@x");
        m.headers_extra
            .insert("auto-submitted".into(), "auto-replied".into());
        m.headers_extra.insert("list-id".into(), "<l@x>".into());
        m.headers_extra.insert("x-spam-flag".into(), "YES".into());
        m.headers_extra
            .insert("feedback-id".into(), "fbl:abc".into());
        m.headers_extra
            .insert("x-mailer".into(), "Mailchimp Mailer".into());
        let cfg = LoopPreventionCfg {
            auto_submitted: false,
            list_headers: false,
            self_from: false,
            spam_flag: false,
            feedback_id: false,
            esp_mailer: false,
        };
        assert_eq!(should_skip(&m, "ops@x", &cfg), None);
    }

    #[test]
    fn x_spam_flag_yes_skips() {
        let mut m = meta_from("alice@x");
        m.headers_extra.insert("x-spam-flag".into(), "YES".into());
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::SpamFlag)
        );
    }

    #[test]
    fn x_spam_flag_no_does_not_skip() {
        let mut m = meta_from("alice@x");
        m.headers_extra.insert("x-spam-flag".into(), "NO".into());
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), None);
    }

    #[test]
    fn feedback_id_skips() {
        let mut m = meta_from("alice@x");
        m.headers_extra
            .insert("feedback-id".into(), "1:campaign:provider".into());
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::FeedbackId)
        );
    }

    #[test]
    fn x_mailer_mailchimp_skips() {
        let mut m = meta_from("alice@x");
        m.headers_extra
            .insert("x-mailer".into(), "Mailchimp Mailer - **CID**".into());
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::EspMailer)
        );
    }

    #[test]
    fn x_mailer_sendgrid_skips_case_insensitive() {
        let mut m = meta_from("alice@x");
        m.headers_extra
            .insert("x-mailer".into(), "SENDGRID/1.0".into());
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::EspMailer)
        );
    }

    #[test]
    fn x_mailer_human_client_does_not_skip() {
        let mut m = meta_from("alice@x");
        m.headers_extra
            .insert("x-mailer".into(), "Apple Mail (2.3654.120.0.1.13)".into());
        assert_eq!(should_skip(&m, "ops@x", &cfg_all()), None);
    }

    #[test]
    fn auto_submitted_wins_over_list_id() {
        // Both signals present; auto_submitted is checked first.
        let mut m = meta_from("alice@x");
        m.headers_extra
            .insert("auto-submitted".into(), "auto-replied".into());
        m.headers_extra.insert("list-id".into(), "<l@x>".into());
        assert_eq!(
            should_skip(&m, "ops@x", &cfg_all()),
            Some(SkipReason::AutoSubmitted)
        );
    }
}
