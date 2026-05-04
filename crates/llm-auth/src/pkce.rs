//! PKCE (RFC 7636) primitives shared by every OAuth flow in this
//! crate.
//!
//! Two state-encoding flavors live here because upstream providers
//! disagree about what `state` characters they tolerate:
//!
//! * **`StateEncoding::HexOnly`** — Anthropic's authorize endpoint
//!   echoes `state` straight into URL fragments without re-escaping;
//!   any `-`/`_`/`=` triggers a generic "Invalid request format". Use
//!   `[0-9a-f]` for the state on Anthropic flows.
//! * **`StateEncoding::Base64Url`** — MiniMax accepts the standard
//!   url-safe-no-pad alphabet.
//!
//! No I/O lives here. The wizard CLI and the admin RPC handler both
//! call [`gen_pkce`] and [`parse_code_payload`].

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// PKCE verifier + challenge + opaque CSRF state.
///
/// `verifier` and `state` are sensitive — leaking them lets an
/// attacker exchange an intercepted authorization code for tokens.
/// The crate never logs them; callers must do the same.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Pkce {
    /// Cryptographic random ≥ 32 bytes, base64url-no-pad.
    pub verifier: String,
    /// `BASE64URL(SHA256(verifier))`.
    pub challenge: String,
    /// CSRF token returned to the operator and echoed back via the
    /// authorization redirect. Validated on exchange.
    pub state: String,
}

/// Discriminator for the two state-encoding flavors that the
/// upstream OAuth providers tolerate. Anthropic forces hex-only;
/// MiniMax accepts base64url.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEncoding {
    /// `[0-9a-f]` only. Required for Anthropic.
    HexOnly,
    /// Standard URL-safe-no-pad alphabet. Used by MiniMax.
    Base64Url,
}

/// Generate a fresh PKCE bundle suitable for one OAuth flow.
///
/// The verifier comes from 32 cryptographic-random bytes; the
/// challenge is `BASE64URL(SHA256(verifier))` per RFC 7636 §4.2 with
/// `code_challenge_method=S256`. The state is 16 random bytes
/// encoded per `encoding`.
pub fn gen_pkce(encoding: StateEncoding) -> Pkce {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));

    let mut state_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut state_bytes);
    let state = match encoding {
        StateEncoding::HexOnly => state_bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        StateEncoding::Base64Url => {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state_bytes)
        }
    };
    Pkce {
        verifier,
        challenge,
        state,
    }
}

/// Minimal percent-encoding for query-string values: escape anything
/// that isn't unreserved per RFC 3986.
///
/// `+` is used for spaces (application/x-www-form-urlencoded).
/// Strict `%20` is rejected by Anthropic's authorize endpoint with a
/// generic "Invalid request format", so we keep the form-encoding
/// here too — works for both Anthropic and MiniMax.
pub fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Parse the `<code>#<state>` payload an operator pastes back from
/// the OAuth callback page.
///
/// Tolerates four input shapes operators have been observed pasting:
///
/// 1. `code123#state456` — the official format.
/// 2. `code123&state=456` — URL query form.
/// 3. `https://console.anthropic.com/oauth/code/callback?code=abc&state=xyz`
///    — the full callback URL.
/// 4. `#code123#state456` — leading fragment marker.
///
/// Returns `(code, state)` as owned strings.
pub fn parse_code_payload(raw: &str) -> Result<(String, String), ParseCodeError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ParseCodeError::Empty);
    }

    // Full URL — pull the substring after `code=`.
    let core = if trimmed.contains("code=") {
        trimmed
            .split_once("code=")
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_else(|| trimmed.to_string())
    } else {
        trimmed.to_string()
    };

    // Drop a leading `#` fragment marker if the operator copied it.
    let core = core.trim_start_matches('#').to_string();
    let (code, state) = core
        .split_once('#')
        .or_else(|| core.split_once('&'))
        .ok_or(ParseCodeError::MissingState)?;

    // The state portion may carry `state=` if pasted from a URL.
    let state = state.trim_start_matches("state=").to_string();
    let code = code.trim().to_string();
    let state = state.trim().to_string();
    if code.is_empty() || state.is_empty() {
        return Err(ParseCodeError::MissingState);
    }
    Ok((code, state))
}

/// Errors from [`parse_code_payload`].
#[derive(Debug, thiserror::Error)]
pub enum ParseCodeError {
    /// The input was empty after trimming.
    #[error("empty payload — paste `<code>#<state>` from the callback page")]
    Empty,
    /// The expected `<code>#<state>` separator was missing.
    #[error("invalid format — expected `<code>#<state>`")]
    MissingState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_base64url_sha256_of_verifier() {
        let p = gen_pkce(StateEncoding::HexOnly);
        let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(p.verifier.as_bytes()));
        assert_eq!(p.challenge, expect);
    }

    #[test]
    fn hex_state_is_hex_only() {
        let p = gen_pkce(StateEncoding::HexOnly);
        assert!(
            p.state.chars().all(|c| c.is_ascii_hexdigit()),
            "expected hex-only, got: {}",
            p.state
        );
        assert_eq!(p.state.len(), 32, "16 bytes hex-encoded = 32 chars");
    }

    #[test]
    fn base64url_state_avoids_padding() {
        let p = gen_pkce(StateEncoding::Base64Url);
        assert!(
            !p.state.contains('='),
            "url-safe-no-pad must not contain `=`"
        );
        assert!(
            !p.state.contains('+') && !p.state.contains('/'),
            "url-safe alphabet should not contain `+/`"
        );
    }

    #[test]
    fn pct_encode_escapes_unreserved_only() {
        assert_eq!(pct_encode("abc-_.~XYZ"), "abc-_.~XYZ");
        assert_eq!(pct_encode("hello world"), "hello+world");
        assert_eq!(pct_encode("a/b:c"), "a%2Fb%3Ac");
    }

    #[test]
    fn parse_code_payload_accepts_hash_form() {
        let (c, s) = parse_code_payload("abc123#mystate").unwrap();
        assert_eq!(c, "abc123");
        assert_eq!(s, "mystate");
    }

    #[test]
    fn parse_code_payload_accepts_full_url() {
        let url = "https://console.anthropic.com/oauth/code/callback?code=abc123&state=mystate";
        let (c, s) = parse_code_payload(url).unwrap();
        assert_eq!(c, "abc123");
        assert_eq!(s, "mystate");
    }

    #[test]
    fn parse_code_payload_accepts_query_form() {
        let (c, s) = parse_code_payload("code123&state=xyz").unwrap();
        assert_eq!(c, "code123");
        assert_eq!(s, "xyz");
    }

    #[test]
    fn parse_code_payload_strips_leading_fragment() {
        let (c, s) = parse_code_payload("#abc#def").unwrap();
        assert_eq!(c, "abc");
        assert_eq!(s, "def");
    }

    #[test]
    fn parse_code_payload_rejects_empty() {
        assert!(matches!(
            parse_code_payload("   "),
            Err(ParseCodeError::Empty)
        ));
    }

    #[test]
    fn parse_code_payload_rejects_missing_state() {
        assert!(matches!(
            parse_code_payload("only-code-no-state"),
            Err(ParseCodeError::MissingState)
        ));
    }
}
