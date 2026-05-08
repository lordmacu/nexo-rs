//! JID parsing + canonicalisation helpers for the WhatsApp /
//! SMS identity surface (M15.23.e).
//!
//! Mirrors the conventions used by Baileys
//! (`jidNormalizedUser`) + whatsmeow (`ParseJID`):
//!
//! - **No E.164 conversion** — the user portion stays as
//!   raw digits the way both libraries persist it. Adding a
//!   `+` prefix would break exact-match against the JID
//!   string the operator's WA client surfaces.
//! - **Legacy server canonicalisation** — `c.us` (legacy
//!   WhatsApp Business) collapses into `s.whatsapp.net` so
//!   one contact migrating between formats hits the same
//!   `PersonPhone` row.
//! - **Device suffix dropped** — `573001234567:1@s.whatsapp.net`
//!   resolves to the same identity as
//!   `573001234567@s.whatsapp.net`. Multi-device is metadata,
//!   not identity.
//! - **LID + PN coexist** — both server types have their own
//!   namespace; the [`PersonPhoneStore`] exact-match contract
//!   lets the caller persist either form. The
//!   [`super::LidPnMapping`] type (future slice) bridges them
//!   when WA reports a migration.
//!
//! ## Special-cased servers
//!
//! - `s.whatsapp.net` / `c.us` → canonical `s.whatsapp.net`.
//! - `lid` → kept as-is (different namespace).
//! - `g.us` (group), `broadcast`, `status@broadcast`,
//!   `bot` → not a person identifier; helper rejects.

use thiserror::Error;

/// Parsed JID broken into its identity components. Cheap to
/// `Clone`; carries owned strings for ergonomics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedJid {
    /// User portion as raw digits / id chars (no `+`, no
    /// device suffix, lower-cased). Empty for non-user
    /// servers — caller checks [`Self::is_user`] before
    /// using as identity.
    pub user: String,
    /// Canonical server label (`s.whatsapp.net`, `lid`, …).
    /// `c.us` already canonicalised to `s.whatsapp.net`.
    pub server: String,
    /// Device suffix (`:1`, `:2`, …). `None` when the JID
    /// has no suffix. Identity comparisons MUST ignore this.
    pub device: Option<u16>,
}

impl ParsedJid {
    /// `true` when this JID identifies a single human user
    /// (PN or LID). Group / broadcast / bot servers return
    /// `false`.
    pub fn is_user(&self) -> bool {
        matches!(self.server.as_str(), "s.whatsapp.net" | "lid")
    }

    /// Canonical identity string the [`super::PersonPhoneStore`]
    /// uses as its `phone` key. Format: `<user>@<server>` —
    /// device + agent dropped. Empty user (non-user server)
    /// returns `<empty>@<server>` so the caller can spot the
    /// programmer error in logs.
    pub fn canonical(&self) -> String {
        format!("{}@{}", self.user, self.server)
    }

    /// Bare digits (or LID id chars) without the server
    /// suffix. Useful when the caller persists alongside an
    /// E.164 phone the operator entered manually.
    pub fn user_only(&self) -> &str {
        &self.user
    }
}

/// Reasons JID parsing can fail. Caller maps each to a
/// `tracing::warn` + skips the contact — never fatal.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum JidParseError {
    /// Input was empty / whitespace-only.
    #[error("jid empty")]
    Empty,
    /// No `@` separator — not a JID.
    #[error("jid missing '@' separator: {0:?}")]
    NoSeparator(String),
    /// User portion was empty (`@server` only).
    #[error("jid has empty user portion: {0:?}")]
    EmptyUser(String),
    /// Server portion not in the known set.
    #[error("jid server unknown: {0:?}")]
    UnknownServer(String),
    /// Device suffix wasn't a parseable u16.
    #[error("jid device suffix invalid: {0:?}")]
    InvalidDevice(String),
}

const KNOWN_USER_SERVERS: &[&str] = &["s.whatsapp.net", "lid"];
const KNOWN_LEGACY_SERVERS: &[&str] = &["c.us"];
const KNOWN_NON_USER_SERVERS: &[&str] = &[
    "g.us",
    "broadcast",
    "status@broadcast",
    "bot",
    "newsletter",
];

/// Parse a JID string into [`ParsedJid`]. Canonicalises
/// legacy `c.us` to `s.whatsapp.net`; lowercases the user
/// portion; strips the device suffix.
///
/// Group / broadcast / bot JIDs ARE accepted (so the
/// caller can pattern-match on `parsed.is_user()` without
/// double-validation) but unknown servers reject.
pub fn parse_jid(input: &str) -> Result<ParsedJid, JidParseError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(JidParseError::Empty);
    }
    let (user_part, server_part) = raw
        .split_once('@')
        .ok_or_else(|| JidParseError::NoSeparator(raw.to_string()))?;
    if user_part.is_empty() {
        return Err(JidParseError::EmptyUser(raw.to_string()));
    }

    // Strip device suffix `:device` AND legacy agent suffix
    // `.agent`. We accept both whatsmeow (`agent.device`) and
    // Baileys (`user_agent:device`) forms — `:` always wins as
    // the device separator.
    let (user_no_device, device) =
        if let Some((before, dev)) = user_part.rsplit_once(':') {
            let dev_num = dev
                .parse::<u16>()
                .map_err(|_| JidParseError::InvalidDevice(raw.to_string()))?;
            (before, Some(dev_num))
        } else {
            (user_part, None)
        };
    // Drop any agent suffix `_agent` that Baileys threads
    // into the user portion (whatsmeow uses `.agent` instead).
    let user_no_agent = user_no_device
        .split_once('_')
        .map(|(u, _)| u)
        .unwrap_or(user_no_device);
    let user_no_agent = user_no_agent
        .split_once('.')
        .map(|(u, _)| u)
        .unwrap_or(user_no_agent);
    let user = user_no_agent.to_lowercase();

    let canonical_server = if KNOWN_LEGACY_SERVERS.contains(&server_part) {
        // Baileys convention: c.us → s.whatsapp.net.
        "s.whatsapp.net".to_string()
    } else if KNOWN_USER_SERVERS.contains(&server_part)
        || KNOWN_NON_USER_SERVERS.contains(&server_part)
    {
        server_part.to_lowercase()
    } else {
        return Err(JidParseError::UnknownServer(server_part.to_string()));
    };

    Ok(ParsedJid {
        user,
        server: canonical_server,
        device,
    })
}

/// Convenience: parse + return the canonical
/// `<user>@<server>` string. Most callers want this — the
/// full [`ParsedJid`] is only useful when the caller cares
/// about the user vs server split.
pub fn normalize_jid(input: &str) -> Result<String, JidParseError> {
    Ok(parse_jid(input)?.canonical())
}

/// Two JIDs identify the same user when their canonical
/// form (sans device + agent) matches AND the server is the
/// same identity namespace (don't cross PN/LID).
pub fn same_user(a: &ParsedJid, b: &ParsedJid) -> bool {
    a.is_user() && b.is_user() && a.user == b.user && a.server == b.server
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_canonical_pn_jid() {
        let p = parse_jid("573001234567@s.whatsapp.net").unwrap();
        assert_eq!(p.user, "573001234567");
        assert_eq!(p.server, "s.whatsapp.net");
        assert_eq!(p.device, None);
        assert!(p.is_user());
    }

    #[test]
    fn parse_strips_device_suffix() {
        let p = parse_jid("573001234567:5@s.whatsapp.net").unwrap();
        assert_eq!(p.user, "573001234567");
        assert_eq!(p.device, Some(5));
        assert_eq!(p.canonical(), "573001234567@s.whatsapp.net");
    }

    #[test]
    fn parse_canonicalises_legacy_c_us() {
        // Baileys convention — `c.us` is the old WA Business
        // server; both libraries collapse to `s.whatsapp.net`
        // for cross-protocol identity.
        let p = parse_jid("573001234567@c.us").unwrap();
        assert_eq!(p.server, "s.whatsapp.net");
    }

    #[test]
    fn parse_keeps_lid_distinct_from_pn() {
        let pn = parse_jid("573001234567@s.whatsapp.net").unwrap();
        let lid = parse_jid("123456789@lid").unwrap();
        assert!(pn.is_user());
        assert!(lid.is_user());
        // Different namespaces — never collapse.
        assert_ne!(pn.canonical(), lid.canonical());
    }

    #[test]
    fn parse_strips_baileys_agent_underscore() {
        // Baileys threads agent like `user_15:1@server`.
        let p = parse_jid("573001234567_15:1@s.whatsapp.net").unwrap();
        assert_eq!(p.user, "573001234567");
        assert_eq!(p.device, Some(1));
    }

    #[test]
    fn parse_strips_whatsmeow_agent_dot() {
        // Whatsmeow threads agent like `user.15:1@server`.
        let p = parse_jid("573001234567.15:1@s.whatsapp.net").unwrap();
        assert_eq!(p.user, "573001234567");
        assert_eq!(p.device, Some(1));
    }

    #[test]
    fn parse_lower_cases_user() {
        // LID JIDs occasionally contain hex; normalize to
        // lowercase so the exact-match store doesn't trip on
        // case variation.
        let p = parse_jid("ABCDEF@lid").unwrap();
        assert_eq!(p.user, "abcdef");
    }

    #[test]
    fn parse_group_jid_accepted_but_not_user() {
        let p = parse_jid("123456-987654@g.us").unwrap();
        assert_eq!(p.server, "g.us");
        assert!(!p.is_user());
    }

    #[test]
    fn parse_status_broadcast_not_user() {
        let p = parse_jid("status@broadcast").unwrap();
        assert!(!p.is_user());
    }

    #[test]
    fn parse_unknown_server_rejected() {
        let r = parse_jid("123@example.com");
        assert!(matches!(r, Err(JidParseError::UnknownServer(_))));
    }

    #[test]
    fn parse_empty_input_rejected() {
        assert_eq!(parse_jid("").unwrap_err(), JidParseError::Empty);
        assert_eq!(parse_jid("   ").unwrap_err(), JidParseError::Empty);
    }

    #[test]
    fn parse_no_separator_rejected() {
        let r = parse_jid("573001234567");
        assert!(matches!(r, Err(JidParseError::NoSeparator(_))));
    }

    #[test]
    fn parse_empty_user_rejected() {
        let r = parse_jid("@s.whatsapp.net");
        assert!(matches!(r, Err(JidParseError::EmptyUser(_))));
    }

    #[test]
    fn parse_invalid_device_rejected() {
        let r = parse_jid("573001234567:abc@s.whatsapp.net");
        assert!(matches!(r, Err(JidParseError::InvalidDevice(_))));
    }

    #[test]
    fn normalize_jid_returns_canonical_string() {
        let s = normalize_jid("573001234567:1@c.us").unwrap();
        // Device dropped, c.us → s.whatsapp.net.
        assert_eq!(s, "573001234567@s.whatsapp.net");
    }

    #[test]
    fn same_user_ignores_device_and_agent() {
        let a = parse_jid("573001234567:1@s.whatsapp.net").unwrap();
        let b = parse_jid("573001234567:5@s.whatsapp.net").unwrap();
        assert!(same_user(&a, &b));
    }

    #[test]
    fn same_user_rejects_cross_namespace() {
        let pn = parse_jid("573001234567@s.whatsapp.net").unwrap();
        let lid = parse_jid("573001234567@lid").unwrap();
        // Don't collapse PN ↔ LID without an explicit
        // mapping — the caller's `LidPnMapping` is the
        // bridge.
        assert!(!same_user(&pn, &lid));
    }

    #[test]
    fn same_user_rejects_groups() {
        let g1 = parse_jid("12-34@g.us").unwrap();
        let g2 = parse_jid("12-34@g.us").unwrap();
        // Groups aren't user identities.
        assert!(!same_user(&g1, &g2));
    }
}
