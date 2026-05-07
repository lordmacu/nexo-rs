use once_cell::sync::Lazy;
use std::collections::HashSet;

pub use nexo_tool_meta::marketing::DomainKind;

/// Curated list of public personal email providers. Tenant-
/// agnostic — every empresa shares this classification because
/// "gmail.com is personal" is a global fact. Updated via SDK
/// minor releases.
const PERSONAL_PROVIDERS: &[&str] = &[
    // Major US/global
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "outlook.es",
    "hotmail.com",
    "hotmail.es",
    "hotmail.co.uk",
    "live.com",
    "live.es",
    "msn.com",
    "yahoo.com",
    "yahoo.es",
    "yahoo.co.uk",
    "yahoo.fr",
    "yahoo.de",
    "yahoo.com.mx",
    "yahoo.com.ar",
    "yahoo.com.br",
    "ymail.com",
    "rocketmail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "aol.es",
    // Privacy / pro-personal
    "protonmail.com",
    "proton.me",
    "pm.me",
    "tutanota.com",
    "tutanota.de",
    "tuta.io",
    "fastmail.com",
    "fastmail.fm",
    "hey.com",
    "duck.com",
    // Latam-popular ISPs
    "yahoo.es",
    "terra.com.mx",
    "terra.com.br",
    "terra.com.ar",
    "uol.com.br",
    "bol.com.br",
    "globo.com",
    "ig.com.br",
    "yandex.com",
    "yandex.ru",
    "mail.ru",
    "mail.com",
    // Asia-popular
    "qq.com",
    "163.com",
    "126.com",
    "sina.com",
    "naver.com",
    "daum.net",
    "hanmail.net",
    // EU-popular
    "gmx.com",
    "gmx.de",
    "gmx.es",
    "gmx.net",
    "web.de",
    "t-online.de",
    "freenet.de",
    "wanadoo.fr",
    "orange.fr",
    "free.fr",
    "laposte.net",
    "libero.it",
    "tiscali.it",
    "alice.it",
    "virgilio.it",
    "comcast.net",
    "verizon.net",
    "att.net",
    "sbcglobal.net",
    "bellsouth.net",
    "earthlink.net",
    "cox.net",
    "charter.net",
    // Other commonly-seen
    "zoho.com",
    "rediffmail.com",
    "lycos.com",
    "inbox.com",
    "rambler.ru",
];

/// Curated list of disposable / throwaway email hosts.
/// Inbound from these is almost always spam / abuse / probe;
/// the operator's routing rules typically `Drop` them.
const DISPOSABLE_PROVIDERS: &[&str] = &[
    "mailinator.com",
    "mailinator.net",
    "guerrillamail.com",
    "guerrillamail.net",
    "10minutemail.com",
    "10minutemail.net",
    "tempmail.com",
    "temp-mail.org",
    "throwaway.email",
    "yopmail.com",
    "trashmail.com",
    "mintemail.com",
    "fakemail.fr",
    "getnada.com",
    "maildrop.cc",
    "sharklasers.com",
    "spam4.me",
    "dispostable.com",
    "tempmailer.com",
    "tempinbox.com",
    "harakirimail.com",
    "tempr.email",
    "discard.email",
    "mailcatch.com",
    "throwam.com",
    "mailnesia.com",
    "emailondeck.com",
    "byom.de",
    "email-fake.com",
    "fakeinbox.com",
];

static PERSONAL_SET: Lazy<HashSet<&'static str>> =
    Lazy::new(|| PERSONAL_PROVIDERS.iter().copied().collect());

static DISPOSABLE_SET: Lazy<HashSet<&'static str>> =
    Lazy::new(|| DISPOSABLE_PROVIDERS.iter().copied().collect());

/// Classify a domain (or full email — the function strips the
/// local-part and casts to lowercase for you). Unknown domains
/// fall through to `DomainKind::Corporate`.
pub fn classify(domain_or_email: &str) -> DomainKind {
    let lower = domain_or_email.to_ascii_lowercase();
    let domain = match lower.rsplit_once('@') {
        Some((_, d)) => d,
        None => lower.as_str(),
    };
    let domain = domain.trim();
    if DISPOSABLE_SET.contains(domain) {
        return DomainKind::Disposable;
    }
    if PERSONAL_SET.contains(domain) {
        return DomainKind::Personal;
    }
    DomainKind::Corporate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_gmail_as_personal() {
        assert_eq!(classify("gmail.com"), DomainKind::Personal);
        assert_eq!(classify("juan@gmail.com"), DomainKind::Personal);
        assert_eq!(classify("MARIA@GMAIL.COM"), DomainKind::Personal);
    }

    #[test]
    fn classifies_outlook_yahoo_proton_as_personal() {
        assert_eq!(classify("outlook.com"), DomainKind::Personal);
        assert_eq!(classify("yahoo.com.ar"), DomainKind::Personal);
        assert_eq!(classify("proton.me"), DomainKind::Personal);
    }

    #[test]
    fn classifies_mailinator_as_disposable() {
        assert_eq!(classify("mailinator.com"), DomainKind::Disposable);
        assert_eq!(classify("test@10minutemail.com"), DomainKind::Disposable);
    }

    #[test]
    fn unknown_domain_falls_through_to_corporate() {
        assert_eq!(classify("acme.com"), DomainKind::Corporate);
        assert_eq!(classify("globex.io"), DomainKind::Corporate);
        assert_eq!(classify("juan@miempresa.co"), DomainKind::Corporate);
    }

    #[test]
    fn whitespace_trimmed() {
        assert_eq!(classify("  gmail.com  "), DomainKind::Personal);
    }

    #[test]
    fn email_without_at_treated_as_domain() {
        assert_eq!(classify("acme.com"), DomainKind::Corporate);
    }

    #[test]
    fn personal_set_size_reasonable() {
        // Sanity — keep the curated list well above the
        // bare-minimum 20 so operators get good coverage.
        assert!(PERSONAL_PROVIDERS.len() >= 60);
        assert!(DISPOSABLE_PROVIDERS.len() >= 25);
    }
}
