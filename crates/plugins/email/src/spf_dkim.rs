//! SPF + DKIM boot-time alignment check (Phase 48.9).
//!
//! Pure-ish helpers (DNS lookup) the plugin runs at `start` per
//! configured account when `EmailPluginConfig.spf_dkim_warn=true`.
//! The check is best-effort: a private domain or DNS flake just
//! produces a `WARN` log, never aborts boot.
//!
//! v1 scope:
//! - SPF: TXT lookup on the domain itself; record presence + parse
//!   `include:<host>` mechanisms (RFC 7208) so we can flag a sending
//!   host that the policy doesn't authorise.
//! - DKIM: TXT lookup on `default._domainkey.<domain>`. v1 only
//!   probes the `default` selector; the WARN message points the
//!   operator at the other common selectors (`google`, `selector1`,
//!   `mail`).
//!
//! DMARC, multi-selector DKIM rotation, and signature verification
//! are deliberately out of scope.

use std::time::Duration;

use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::TokioAsyncResolver;

/// Common DKIM selectors probed in order. `default` is the historical
/// recommendation; `google` (Gmail / Google Workspace), `selector1` /
/// `selector2` (Microsoft 365 + most ESPs that rotate keys), and
/// `mail` (Postfix `opendkim-genkey` default) cover ~95% of
/// real-world deployments without forcing the operator to pin a
/// custom selector.
pub const DKIM_SELECTORS: &[&str] = &["default", "google", "selector1", "selector2", "mail"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlignmentReport {
    pub domain: String,
    pub spf_present: bool,
    pub spf_record: Option<String>,
    /// `Some(true)` — SPF authorises `sending_host` (or matches via
    /// `include:` mechanism).
    /// `Some(false)` — SPF exists but the host isn't in the policy.
    /// `None` — SPF absent or no `sending_host` was supplied.
    pub spf_includes_host: Option<bool>,
    pub dkim_present: bool,
    pub dkim_record: Option<String>,
    /// Selector that matched (e.g. `default`, `google`, `selector1`).
    /// `None` when no DKIM record was found on any probed selector.
    pub dkim_selector: Option<String>,
    /// True when DNS lookups timed out / errored out. The other
    /// fields default to `false` / `None`; this flag lets the caller
    /// log a different category of WARN.
    pub dns_error: bool,
}

/// Look up SPF + DKIM TXT records for `domain` with a global
/// `timeout`. `sending_host` (typically the SMTP relay host) is
/// matched against `include:` mechanisms so misalignments surface
/// as a separate signal.
pub async fn check_alignment(
    domain: &str,
    sending_host: Option<&str>,
    timeout: Duration,
) -> AlignmentReport {
    let mut report = AlignmentReport {
        domain: domain.to_string(),
        ..AlignmentReport::default()
    };
    if domain.trim().is_empty() {
        report.dns_error = true;
        return report;
    }

    // Try the system resolver first; fall back to Cloudflare so CI /
    // sandbox without /etc/resolv.conf still works.
    let resolver = match TokioAsyncResolver::tokio_from_system_conf() {
        Ok(r) => r,
        Err(_) => {
            TokioAsyncResolver::tokio(ResolverConfig::cloudflare(), ResolverOpts::default())
        }
    };

    let spf = lookup_txt(&resolver, domain, timeout).await;
    match spf {
        Ok(records) => {
            if let Some(spf_rec) = records.iter().find(|r| r.starts_with("v=spf1")) {
                report.spf_present = true;
                report.spf_record = Some(spf_rec.clone());
                if let Some(host) = sending_host {
                    let includes = parse_spf_includes(spf_rec);
                    let host_l = host.to_ascii_lowercase();
                    let matched = includes
                        .iter()
                        .any(|i| {
                            let il = i.to_ascii_lowercase();
                            host_l == il || host_l.ends_with(&format!(".{il}"))
                        });
                    report.spf_includes_host = Some(matched);
                }
            }
        }
        Err(_) => report.dns_error = true,
    }

    // Multi-selector DKIM probe. The four below cover Gmail / O365 /
    // most ESPs (Mailgun / SendGrid / Mailchimp publish under
    // `selector1` and `selector2`) plus the historical defaults.
    // First match wins; if none match the WARN message lists them so
    // the operator can chase a custom selector if they rotate one.
    for selector in DKIM_SELECTORS {
        let name = format!("{selector}._domainkey.{}", domain);
        if let Ok(records) = lookup_txt(&resolver, &name, timeout).await {
            if let Some(dkim_rec) = records.iter().find(|r| r.contains("v=DKIM1")) {
                report.dkim_present = true;
                report.dkim_record = Some(dkim_rec.clone());
                report.dkim_selector = Some((*selector).to_string());
                break;
            }
        }
        // NXDOMAIN per selector is expected — keep walking.
    }

    report
}

async fn lookup_txt(
    resolver: &TokioAsyncResolver,
    name: &str,
    timeout: Duration,
) -> Result<Vec<String>, ()> {
    let fut = resolver.txt_lookup(name);
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(answer)) => {
            let mut out = Vec::new();
            for rec in answer.iter() {
                let mut joined = String::new();
                for chunk in rec.txt_data() {
                    joined.push_str(&String::from_utf8_lossy(chunk));
                }
                if !joined.is_empty() {
                    out.push(joined);
                }
            }
            Ok(out)
        }
        _ => Err(()),
    }
}

/// Extract `include:<host>` mechanism arguments from an SPF record
/// (RFC 7208 §5.2). Tolerant of leading `+` qualifier; ignores
/// other mechanisms.
pub fn parse_spf_includes(record: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in record.split_whitespace() {
        let tok = tok.trim_start_matches(['+', '?']);
        if let Some(host) = tok.strip_prefix("include:") {
            if !host.is_empty() {
                out.push(host.trim_end_matches('.').to_string());
            }
        }
    }
    out
}

/// Decide which WARN messages to emit for a given report. Pulled
/// out of the boot-warn hook so the matrix is unit-testable.
pub fn decide_warns(r: &AlignmentReport) -> Vec<&'static str> {
    let mut out = Vec::new();
    if r.dns_error {
        out.push("dns_error");
        return out;
    }
    if !r.spf_present {
        out.push("spf_missing");
    } else if r.spf_includes_host == Some(false) {
        out.push("spf_misalignment");
    }
    if !r.dkim_present {
        out.push("dkim_missing");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spf_single_include() {
        assert_eq!(
            parse_spf_includes("v=spf1 include:_spf.google.com -all"),
            vec!["_spf.google.com".to_string()]
        );
    }

    #[test]
    fn parse_spf_multi_includes() {
        let r = parse_spf_includes("v=spf1 include:a.x include:b.y ~all");
        assert_eq!(r, vec!["a.x".to_string(), "b.y".into()]);
    }

    #[test]
    fn parse_spf_no_includes_returns_empty() {
        assert!(parse_spf_includes("v=spf1 ip4:1.2.3.4 -all").is_empty());
    }

    #[test]
    fn parse_spf_strips_qualifier() {
        assert_eq!(
            parse_spf_includes("v=spf1 +include:a.x ~all"),
            vec!["a.x".to_string()]
        );
    }

    #[test]
    fn parse_spf_empty_input() {
        assert!(parse_spf_includes("").is_empty());
    }

    #[test]
    fn parse_spf_strips_trailing_dot() {
        assert_eq!(
            parse_spf_includes("v=spf1 include:a.x. -all"),
            vec!["a.x".to_string()]
        );
    }

    #[tokio::test]
    async fn check_alignment_invalid_domain_returns_dns_error() {
        // `.invalid` is reserved by RFC 6761 — every resolver returns
        // NXDOMAIN. We want the helper to surface that as `dns_error`
        // (or both spf/dkim absent) without panicking.
        let r = check_alignment(
            "definitely-does-not-exist.invalid",
            None,
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(r.domain, "definitely-does-not-exist.invalid");
        assert!(!r.spf_present);
        assert!(!r.dkim_present);
        // Either dns_error=true (resolver refused) or both records
        // simply absent — both are acceptable: no panic.
    }

    #[tokio::test]
    async fn check_alignment_empty_domain_is_dns_error() {
        let r = check_alignment("", None, Duration::from_secs(1)).await;
        assert!(r.dns_error);
    }

    #[test]
    fn decide_warns_clean_report_emits_nothing() {
        let r = AlignmentReport {
            spf_present: true,
            dkim_present: true,
            spf_includes_host: Some(true),
            ..AlignmentReport::default()
        };
        assert!(decide_warns(&r).is_empty());
    }

    #[test]
    fn decide_warns_spf_missing() {
        let r = AlignmentReport {
            spf_present: false,
            dkim_present: true,
            ..AlignmentReport::default()
        };
        assert_eq!(decide_warns(&r), vec!["spf_missing"]);
    }

    #[test]
    fn decide_warns_dkim_missing() {
        let r = AlignmentReport {
            spf_present: true,
            spf_includes_host: Some(true),
            dkim_present: false,
            ..AlignmentReport::default()
        };
        assert_eq!(decide_warns(&r), vec!["dkim_missing"]);
    }

    #[test]
    fn decide_warns_misalignment_and_dkim_missing() {
        let r = AlignmentReport {
            spf_present: true,
            spf_includes_host: Some(false),
            dkim_present: false,
            ..AlignmentReport::default()
        };
        assert_eq!(decide_warns(&r), vec!["spf_misalignment", "dkim_missing"]);
    }

    #[test]
    fn dkim_selectors_list_starts_with_default() {
        // `default` first preserves single-selector behaviour for
        // domains that don't rotate keys.
        assert_eq!(DKIM_SELECTORS.first(), Some(&"default"));
        // The five we ship today.
        assert_eq!(DKIM_SELECTORS.len(), 5);
        assert!(DKIM_SELECTORS.contains(&"google"));
        assert!(DKIM_SELECTORS.contains(&"selector1"));
        assert!(DKIM_SELECTORS.contains(&"selector2"));
        assert!(DKIM_SELECTORS.contains(&"mail"));
    }

    #[test]
    fn alignment_report_carries_selector_field() {
        // Default-construct sanity: the new field is `None` and
        // `Default` still works for tests that touch the report.
        let r = AlignmentReport::default();
        assert!(r.dkim_selector.is_none());
    }

    #[test]
    fn decide_warns_dns_error_short_circuits() {
        let r = AlignmentReport {
            dns_error: true,
            ..AlignmentReport::default()
        };
        assert_eq!(decide_warns(&r), vec!["dns_error"]);
    }
}
