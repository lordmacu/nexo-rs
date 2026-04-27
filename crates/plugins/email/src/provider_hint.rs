//! Provider auto-detect (Phase 48.9).
//!
//! Tiny lookup table mapping an address domain to canonical IMAP /
//! SMTP endpoints + provider tag. Covers the four high-traffic
//! consumer providers; everything else falls through to `Custom`
//! with empty hosts (the setup wizard prompts the operator manually).

use nexo_config::types::plugins::{EmailProvider, TlsMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHint {
    pub provider: EmailProvider,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: TlsMode,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: TlsMode,
    /// True when the provider is Gmail and the wizard should
    /// suggest reusing the operator's existing `google-auth.yaml`
    /// account instead of asking for an IMAP password.
    pub suggest_oauth_google: bool,
}

/// Match `domain` (lowercased) against the known providers.
/// Returns the matching `ProviderHint`, or a `Custom` shell with
/// empty hosts when nothing matches.
pub fn provider_hint(domain: &str) -> ProviderHint {
    let d = domain.trim().to_ascii_lowercase();
    match d.as_str() {
        "gmail.com" | "googlemail.com" => ProviderHint {
            provider: EmailProvider::Gmail,
            imap_host: "imap.gmail.com".into(),
            imap_port: 993,
            imap_tls: TlsMode::ImplicitTls,
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 587,
            smtp_tls: TlsMode::Starttls,
            suggest_oauth_google: true,
        },
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" => ProviderHint {
            provider: EmailProvider::Outlook,
            imap_host: "outlook.office365.com".into(),
            imap_port: 993,
            imap_tls: TlsMode::ImplicitTls,
            smtp_host: "smtp.office365.com".into(),
            smtp_port: 587,
            smtp_tls: TlsMode::Starttls,
            suggest_oauth_google: false,
        },
        "yahoo.com" | "yahoo.co.uk" | "ymail.com" | "rocketmail.com" => ProviderHint {
            provider: EmailProvider::Yahoo,
            imap_host: "imap.mail.yahoo.com".into(),
            imap_port: 993,
            imap_tls: TlsMode::ImplicitTls,
            smtp_host: "smtp.mail.yahoo.com".into(),
            smtp_port: 587,
            smtp_tls: TlsMode::Starttls,
            suggest_oauth_google: false,
        },
        "icloud.com" | "me.com" | "mac.com" => ProviderHint {
            provider: EmailProvider::Icloud,
            imap_host: "imap.mail.me.com".into(),
            imap_port: 993,
            imap_tls: TlsMode::ImplicitTls,
            smtp_host: "smtp.mail.me.com".into(),
            smtp_port: 587,
            smtp_tls: TlsMode::Starttls,
            suggest_oauth_google: false,
        },
        _ => ProviderHint {
            provider: EmailProvider::Custom,
            imap_host: String::new(),
            imap_port: 993,
            imap_tls: TlsMode::ImplicitTls,
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_tls: TlsMode::Starttls,
            suggest_oauth_google: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_preset() {
        let h = provider_hint("gmail.com");
        assert!(matches!(h.provider, EmailProvider::Gmail));
        assert_eq!(h.imap_host, "imap.gmail.com");
        assert_eq!(h.smtp_port, 587);
        assert!(h.suggest_oauth_google);
    }

    #[test]
    fn googlemail_alias_maps_to_gmail() {
        assert!(matches!(
            provider_hint("googlemail.com").provider,
            EmailProvider::Gmail
        ));
    }

    #[test]
    fn outlook_preset_uses_office365() {
        let h = provider_hint("outlook.com");
        assert!(matches!(h.provider, EmailProvider::Outlook));
        assert_eq!(h.imap_host, "outlook.office365.com");
        assert!(!h.suggest_oauth_google);
    }

    #[test]
    fn yahoo_preset() {
        let h = provider_hint("yahoo.co.uk");
        assert!(matches!(h.provider, EmailProvider::Yahoo));
        assert_eq!(h.imap_host, "imap.mail.yahoo.com");
    }

    #[test]
    fn icloud_preset_handles_mac_alias() {
        assert!(matches!(
            provider_hint("mac.com").provider,
            EmailProvider::Icloud
        ));
    }

    #[test]
    fn unknown_domain_falls_through_to_custom() {
        let h = provider_hint("example.com");
        assert!(matches!(h.provider, EmailProvider::Custom));
        assert!(h.imap_host.is_empty());
        assert_eq!(h.imap_port, 993);
        assert_eq!(h.smtp_port, 587);
    }

    #[test]
    fn case_insensitive_lookup() {
        assert!(matches!(
            provider_hint("GMAIL.COM").provider,
            EmailProvider::Gmail
        ));
    }
}
