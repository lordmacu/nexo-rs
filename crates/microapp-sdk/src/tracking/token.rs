//! HMAC-signed URL tokens for tracking pixel + click ingest.
//!
//! Threat model: forged URLs (someone hits `/t/o/<msg>?tag=...`
//! with a guessed tag), replayed URLs (caller pings the same
//! ingest route 1M times), tenant-substitution attacks (token
//! minted for tenant A pasted into tenant B's URL).
//!
//! Mitigation: HMAC-SHA256 over `b"\x01" || tenant_id || NUL ||
//! msg_id || NUL || link_id?` — truncated to 16 bytes,
//! base64url no-padding. Constant-time tag compare via
//! [`subtle::ConstantTimeEq`] guards against timing oracles.
//! Forgery requires a 2¹²⁸ search; replay protection lives in
//! the (caller-owned) idempotency layer.
//!
//! ### Format
//!
//! ```text
//! base64url(hmac_sha256(secret, b"\x01" || tenant_id || NUL || msg_id || NUL || link_id?)[..16])
//! ```
//!
//! Example tag: `MFy0MgmShG_LYCOdCxiPeQ` (22 chars, no `+/=`).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

use super::types::{LinkId, MsgId};

type HmacSha256 = Hmac<Sha256>;

/// Version byte prefix on the HMAC input. Bump when the
/// signing format changes (e.g. switch to BLAKE3) so old
/// tokens reject cleanly instead of silently mis-verifying.
const VERSION: u8 = 0x01;

/// Truncation length (bytes). 16 bytes = 128 bits of forgery
/// resistance — same security margin as AES-128. Keeps the
/// base64url tag at 22 chars (URL-friendly, fits in a tweet).
const TAG_LEN: usize = 16;

/// Reasons a tag verification can fail. Encoded errors map to
/// HTTP 401 in the ingest route — never leak which step failed
/// to the client (just "not authorised").
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TokenError {
    /// The base64url payload didn't decode.
    #[error("token: base64 decode failed")]
    BadEncoding,
    /// Decoded bytes were the wrong length.
    #[error("token: wrong length")]
    BadLength,
    /// HMAC verify failed (forged or wrong tenant / msg id).
    #[error("token: tag mismatch")]
    TagMismatch,
}

/// HMAC-signed token. Newtype around the base64url string so
/// the caller can't accidentally pass an arbitrary `&str`
/// where a verified token is expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackingToken(String);

impl TrackingToken {
    /// Wrap a caller-supplied tag (e.g. parsed from URL).
    /// Doesn't verify — call [`TrackingTokenSigner::verify`].
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying base64url string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TrackingToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// HMAC token signer. Holds the per-microapp secret in memory.
///
/// Pick the secret from a high-entropy source (32+ random bytes,
/// hex/base64-encoded in the operator's secrets file). Rotate
/// by reseating the signer Arc + invalidating outstanding tokens
/// in flight (operator's call — old emails with old tags will
/// 401 silently, fine for marketing tracking; not fine for
/// other use cases).
#[derive(Clone)]
pub struct TrackingTokenSigner {
    secret: Vec<u8>,
}

impl std::fmt::Debug for TrackingTokenSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the secret. Length only.
        f.debug_struct("TrackingTokenSigner")
            .field("secret_len", &self.secret.len())
            .finish()
    }
}

impl TrackingTokenSigner {
    /// Build from raw secret bytes. Caller picks the source
    /// (env var, secrets file, KMS, …); SDK refuses anything
    /// shorter than 16 bytes (would weaken the security
    /// margin below 128 bits).
    pub fn new(secret: impl Into<Vec<u8>>) -> Result<Self, TokenError> {
        let secret = secret.into();
        if secret.len() < 16 {
            return Err(TokenError::BadLength);
        }
        Ok(Self { secret })
    }

    /// Sign a `(tenant_id, msg_id)` pair — used by the open
    /// pixel URL.
    pub fn sign_open(&self, tenant_id: &str, msg_id: &MsgId) -> TrackingToken {
        self.sign(tenant_id, msg_id, None)
    }

    /// Sign a `(tenant_id, msg_id, link_id)` triple — used by
    /// the click redirector URL.
    pub fn sign_click(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
        link_id: &LinkId,
    ) -> TrackingToken {
        self.sign(tenant_id, msg_id, Some(link_id))
    }

    /// Verify a tag against the same `(tenant_id, msg_id,
    /// link_id?)` payload. Returns `Ok(())` on match,
    /// `Err(TokenError::TagMismatch)` on forgery — caller maps
    /// every non-`Ok` to HTTP 401.
    pub fn verify(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
        link_id: Option<&LinkId>,
        token: &TrackingToken,
    ) -> Result<(), TokenError> {
        let decoded = URL_SAFE_NO_PAD
            .decode(token.as_str().as_bytes())
            .map_err(|_| TokenError::BadEncoding)?;
        if decoded.len() != TAG_LEN {
            return Err(TokenError::BadLength);
        }
        let expected = self.compute_tag(tenant_id, msg_id, link_id);
        // Constant-time compare so a partial-prefix match leaks
        // zero timing signal.
        if expected.ct_eq(&decoded).into() {
            Ok(())
        } else {
            Err(TokenError::TagMismatch)
        }
    }

    fn sign(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
        link_id: Option<&LinkId>,
    ) -> TrackingToken {
        let tag = self.compute_tag(tenant_id, msg_id, link_id);
        TrackingToken(URL_SAFE_NO_PAD.encode(tag))
    }

    fn compute_tag(
        &self,
        tenant_id: &str,
        msg_id: &MsgId,
        link_id: Option<&LinkId>,
    ) -> [u8; TAG_LEN] {
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .expect("HmacSha256 accepts any key length");
        mac.update(&[VERSION]);
        mac.update(tenant_id.as_bytes());
        mac.update(b"\x00");
        mac.update(msg_id.as_str().as_bytes());
        mac.update(b"\x00");
        if let Some(l) = link_id {
            mac.update(l.as_str().as_bytes());
        }
        let full = mac.finalize().into_bytes();
        let mut out = [0u8; TAG_LEN];
        out.copy_from_slice(&full[..TAG_LEN]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> TrackingTokenSigner {
        TrackingTokenSigner::new(vec![0u8; 32]).unwrap()
    }

    #[test]
    fn sign_open_round_trips() {
        let s = signer();
        let m = MsgId::new("msg-1");
        let tok = s.sign_open("acme", &m);
        assert_eq!(s.verify("acme", &m, None, &tok), Ok(()));
    }

    #[test]
    fn sign_click_round_trips() {
        let s = signer();
        let m = MsgId::new("msg-1");
        let l = LinkId::new("L0");
        let tok = s.sign_click("acme", &m, &l);
        assert_eq!(s.verify("acme", &m, Some(&l), &tok), Ok(()));
    }

    #[test]
    fn cross_tenant_tag_rejected() {
        let s = signer();
        let m = MsgId::new("msg-1");
        let tok = s.sign_open("acme", &m);
        // Same tag, different tenant → mismatch.
        assert_eq!(
            s.verify("globex", &m, None, &tok),
            Err(TokenError::TagMismatch),
        );
    }

    #[test]
    fn cross_msg_tag_rejected() {
        let s = signer();
        let tok = s.sign_open("acme", &MsgId::new("msg-1"));
        assert_eq!(
            s.verify("acme", &MsgId::new("msg-2"), None, &tok),
            Err(TokenError::TagMismatch),
        );
    }

    #[test]
    fn open_tag_rejected_for_click_payload() {
        let s = signer();
        let m = MsgId::new("msg-1");
        let l = LinkId::new("L0");
        // Tag was minted with no link_id; verification with one
        // expects a different HMAC payload.
        let tok = s.sign_open("acme", &m);
        assert_eq!(
            s.verify("acme", &m, Some(&l), &tok),
            Err(TokenError::TagMismatch),
        );
    }

    #[test]
    fn different_secrets_dont_collide() {
        let a = TrackingTokenSigner::new(vec![1u8; 32]).unwrap();
        let b = TrackingTokenSigner::new(vec![2u8; 32]).unwrap();
        let m = MsgId::new("msg-1");
        let tok = a.sign_open("acme", &m);
        assert_eq!(
            b.verify("acme", &m, None, &tok),
            Err(TokenError::TagMismatch),
        );
    }

    #[test]
    fn malformed_token_returns_bad_encoding() {
        let s = signer();
        let bad = TrackingToken::from_string("!!! not base64 !!!");
        assert_eq!(
            s.verify("acme", &MsgId::new("m"), None, &bad),
            Err(TokenError::BadEncoding),
        );
    }

    #[test]
    fn truncated_token_returns_bad_length() {
        let s = signer();
        // Valid base64url, decodes to 1 byte → wrong length.
        let bad = TrackingToken::from_string("aQ");
        assert_eq!(
            s.verify("acme", &MsgId::new("m"), None, &bad),
            Err(TokenError::BadLength),
        );
    }

    #[test]
    fn short_secret_rejected() {
        let r = TrackingTokenSigner::new(vec![0u8; 8]);
        assert_eq!(r.unwrap_err(), TokenError::BadLength);
    }

    #[test]
    fn signer_debug_does_not_leak_secret() {
        let s = TrackingTokenSigner::new(b"super-secret-value-32bytes-okay!".to_vec()).unwrap();
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("super-secret"));
        assert!(dbg.contains("secret_len"));
    }

    #[test]
    fn token_format_is_url_safe() {
        let s = signer();
        let tok = s.sign_open("acme", &MsgId::new("msg-1"));
        let v = tok.as_str();
        // 16 bytes → 22 base64url chars, no padding.
        assert_eq!(v.len(), 22);
        // No URL-unsafe chars.
        assert!(!v.contains('+'));
        assert!(!v.contains('/'));
        assert!(!v.contains('='));
    }
}
