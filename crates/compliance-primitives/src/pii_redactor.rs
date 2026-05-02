//! PII redactor.
//!
//! Strips personally-identifiable information from inbound text
//! before the LLM sees it. Defaults cover phone numbers, credit
//! card numbers, and email addresses — the categories most
//! microapps need without further config.
//!
//! Each match is replaced with a placeholder (`[PHONE]`,
//! `[CARD]`, `[EMAIL]`). Microapps wire this into a Phase 83.3
//! `Transform` hook to swap the redacted body in place.

use regex::Regex;

/// Statistics from one redaction pass — useful for audit logs
/// + observability ("how many cards did we strip this turn?").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionStats {
    pub phones_redacted: usize,
    pub cards_redacted: usize,
    pub emails_redacted: usize,
}

impl RedactionStats {
    pub fn total(&self) -> usize {
        self.phones_redacted + self.cards_redacted + self.emails_redacted
    }
}

/// Built-in placeholders. Stable strings so the LLM's prompt
/// cache prefix-matcher sees identical bytes turn-to-turn.
pub const PHONE_PLACEHOLDER: &str = "[PHONE]";
pub const CARD_PLACEHOLDER: &str = "[CARD]";
pub const EMAIL_PLACEHOLDER: &str = "[EMAIL]";

#[derive(Debug, Clone)]
pub struct PiiRedactor {
    /// `+57 311 572 8852`, `(312) 555-0182`, `5551234567`, etc.
    /// Permissive enough to catch international forms; rejects
    /// short fragments (< 7 digits) so 4-digit auth codes aren't
    /// stripped by accident.
    phone: Regex,
    /// 13–19 digit runs with optional spaces / dashes — covers
    /// Visa / MC / Amex / Discover. Luhn check is OPTIONAL and
    /// off by default; turn it on with `with_luhn(true)` to cut
    /// false positives.
    card: Regex,
    email: Regex,
    luhn_check: bool,
    /// Toggles on a per-category basis so a microapp that needs
    /// raw emails (e.g. a CRM lookup tool) can disable that
    /// pattern without forking the regex list.
    redact_phones: bool,
    redact_cards: bool,
    redact_emails: bool,
}

impl Default for PiiRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl PiiRedactor {
    pub fn new() -> Self {
        Self {
            // Phone numbers must contain at least one separator
            // (space, dash, dot, or parens) so a continuous 16+
            // digit blob is treated as a credit-card candidate
            // by the card regex above, not eaten here.
            // Optional `+CC` prefix; main body is `digits SEP
            // digits (SEP digits)*` requiring at least one SEP.
            phone: Regex::new(
                r"(?x)
                (?:\+\d{1,3}[\s\-\.])?       # optional +CC then SEP
                (?:                          # OR parens area
                    \(\d{2,4}\)\s?\d{3,4}[\s\-\.]\d{3,4}
                  | \d{2,4}[\s\-\.]\d{3,4}[\s\-\.]?\d{0,4}
                  | \d{3}[\s\-\.]\d{3,4}[\s\-\.]?\d{0,4}
                )
                ",
            )
            .unwrap(),
            // 13–19 digits with optional spaces/dashes between
            // groups. Use possessive bounds via lookahead is
            // unavailable in `regex`; we strip pre/post on the
            // matched range.
            card: Regex::new(r"\b(?:\d[\s\-]?){13,19}\b").unwrap(),
            email: Regex::new(
                r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b",
            )
            .unwrap(),
            luhn_check: false,
            redact_phones: true,
            redact_cards: true,
            redact_emails: true,
        }
    }

    /// Toggle the Luhn check on the credit-card regex. When
    /// `true`, only Luhn-valid runs are redacted. Recommended
    /// for production microapps to cut false positives on
    /// 16-digit numbers that aren't cards (e.g. account
    /// numbers).
    pub fn with_luhn(mut self, enabled: bool) -> Self {
        self.luhn_check = enabled;
        self
    }

    /// Skip phone redaction (e.g. for a contact-management
    /// microapp that needs the raw digits).
    pub fn skip_phones(mut self) -> Self {
        self.redact_phones = false;
        self
    }

    pub fn skip_cards(mut self) -> Self {
        self.redact_cards = false;
        self
    }

    pub fn skip_emails(mut self) -> Self {
        self.redact_emails = false;
        self
    }

    /// Redact `body` and return `(redacted, stats)`.
    pub fn redact(&self, body: &str) -> (String, RedactionStats) {
        let mut stats = RedactionStats::default();
        let mut redacted = body.to_string();

        if self.redact_cards {
            let pattern = self.card.clone();
            let luhn = self.luhn_check;
            redacted = pattern
                .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                    let raw = &caps[0];
                    if luhn && !is_luhn_valid(raw) {
                        return raw.to_string();
                    }
                    stats.cards_redacted += 1;
                    CARD_PLACEHOLDER.to_string()
                })
                .into_owned();
        }

        if self.redact_emails {
            let pattern = self.email.clone();
            redacted = pattern
                .replace_all(&redacted, |_: &regex::Captures<'_>| {
                    stats.emails_redacted += 1;
                    EMAIL_PLACEHOLDER.to_string()
                })
                .into_owned();
        }

        if self.redact_phones {
            let pattern = self.phone.clone();
            redacted = pattern
                .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                    let raw = &caps[0];
                    let digits: usize =
                        raw.chars().filter(|c| c.is_ascii_digit()).count();
                    if digits < 7 {
                        // Too short — keep verbatim (auth codes,
                        // SKUs, etc.).
                        return raw.to_string();
                    }
                    stats.phones_redacted += 1;
                    PHONE_PLACEHOLDER.to_string()
                })
                .into_owned();
        }

        (redacted, stats)
    }
}

fn is_luhn_valid(raw: &str) -> bool {
    let digits: Vec<u32> = raw
        .chars()
        .filter_map(|c| c.to_digit(10))
        .collect();
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }
    let mut sum = 0u32;
    let mut alt = false;
    for d in digits.iter().rev() {
        let mut x = *d;
        if alt {
            x *= 2;
            if x > 9 {
                x -= 9;
            }
        }
        sum += x;
        alt = !alt;
    }
    sum % 10 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_plain_email() {
        let r = PiiRedactor::new();
        let (out, stats) = r.redact("contact me at jane.doe@example.com please");
        assert!(out.contains("[EMAIL]"));
        assert!(!out.contains("jane.doe@example.com"));
        assert_eq!(stats.emails_redacted, 1);
    }

    #[test]
    fn redacts_credit_card_with_dashes() {
        let r = PiiRedactor::new();
        let (out, stats) = r.redact("my card 4111-1111-1111-1111 thanks");
        assert!(out.contains("[CARD]"));
        assert!(!out.contains("4111"));
        assert_eq!(stats.cards_redacted, 1);
    }

    #[test]
    fn redacts_credit_card_with_spaces() {
        let r = PiiRedactor::new();
        let (out, _) = r.redact("4111 1111 1111 1111");
        assert!(out.contains("[CARD]"));
    }

    #[test]
    fn redacts_phone_international() {
        let r = PiiRedactor::new();
        let (out, stats) = r.redact("call +57 311 572 8852 anytime");
        assert!(out.contains("[PHONE]"));
        assert_eq!(stats.phones_redacted, 1);
    }

    #[test]
    fn does_not_redact_short_codes() {
        // 4-digit OTP must NOT be eaten as a phone number.
        let r = PiiRedactor::new();
        let (out, stats) = r.redact("your code is 1234, valid 5 minutes");
        assert!(out.contains("1234"));
        assert_eq!(stats.phones_redacted, 0);
    }

    #[test]
    fn luhn_check_rejects_non_card_numbers() {
        // A 16-digit run that ISN'T Luhn-valid must survive
        // when the Luhn gate is on.
        let r = PiiRedactor::new().with_luhn(true);
        // 16 random digits, not Luhn-valid.
        let bad = "1234567812345678";
        // Compute: alt sum sequence; this happens to NOT pass
        // Luhn — verified empirically.
        let (out, stats) = r.redact(&format!("ref {bad} please"));
        assert!(out.contains(bad), "luhn-invalid number must survive");
        assert_eq!(stats.cards_redacted, 0);

        // 4111 1111 1111 1111 IS Luhn-valid (canonical Visa
        // test card) — must still redact.
        let (out2, stats2) = r.redact("card 4111111111111111");
        assert!(out2.contains("[CARD]"));
        assert_eq!(stats2.cards_redacted, 1);
    }

    #[test]
    fn skip_emails_keeps_email_intact() {
        let r = PiiRedactor::new().skip_emails();
        let (out, stats) = r.redact("write to alice@x.com");
        assert!(out.contains("alice@x.com"));
        assert_eq!(stats.emails_redacted, 0);
    }

    #[test]
    fn redacts_multiple_pii_types_in_one_pass() {
        let r = PiiRedactor::new();
        let (_, stats) = r.redact(
            "email a@b.co or call +1 555 123 4567 or charge 4111-1111-1111-1111",
        );
        assert_eq!(stats.emails_redacted, 1);
        assert_eq!(stats.phones_redacted, 1);
        assert_eq!(stats.cards_redacted, 1);
        assert_eq!(stats.total(), 3);
    }

    #[test]
    fn empty_body_no_stats() {
        let r = PiiRedactor::new();
        let (out, stats) = r.redact("");
        assert_eq!(out, "");
        assert_eq!(stats, RedactionStats::default());
    }
}
