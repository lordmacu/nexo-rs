//! HMAC-SHA256 bearer-token issuer.
//!
//! `agent pair start` builds a JSON `TokenClaims` blob, signs it with
//! the secret in `~/.nexo/secret/pairing.key`, and ships
//! `b64u(claims) + "." + b64u(sig)` to the companion. The companion
//! presents this as a `Bearer` token to the daemon's gateway, which
//! verifies HMAC + expiry.

use std::path::Path;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::types::{PairingError, SetupCode, TokenClaims};

type HmacSha256 = Hmac<Sha256>;

pub struct SetupCodeIssuer {
    secret: [u8; 32],
}

impl SetupCodeIssuer {
    /// Open the secret file at `path`, or generate it if missing. On
    /// Unix the new file gets 0600 perms; on other platforms we log
    /// and proceed (paridad con OpenClaw).
    pub fn open_or_create(path: &Path) -> Result<Self, PairingError> {
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut secret = [0u8; 32];
                secret.copy_from_slice(&bytes);
                Ok(Self { secret })
            }
            Ok(_) => Err(PairingError::Invalid("pairing secret has wrong length")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::generate(path),
            Err(e) => Err(PairingError::Io(e.to_string())),
        }
    }

    fn generate(path: &Path) -> Result<Self, PairingError> {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PairingError::Io(e.to_string()))?;
        }
        std::fs::write(path, secret).map_err(|e| PairingError::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .map_err(|e| PairingError::Io(e.to_string()))?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms).map_err(|e| PairingError::Io(e.to_string()))?;
        }
        Ok(Self { secret })
    }

    pub fn issue(
        &self,
        url: &str,
        profile: &str,
        ttl: Duration,
        device_label: Option<&str>,
    ) -> Result<SetupCode, PairingError> {
        if url.trim().is_empty() {
            return Err(PairingError::Invalid("setup-code url is empty"));
        }
        let expires_at = Utc::now()
            + chrono::Duration::from_std(ttl)
                .map_err(|_| PairingError::Invalid("setup-code ttl out of range"))?;
        let mut nonce_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let claims = TokenClaims {
            profile: profile.to_string(),
            expires_at,
            nonce: hex::encode(nonce_bytes),
            device_label: device_label.map(str::to_string),
        };
        let claims_json =
            serde_json::to_vec(&claims).map_err(|e| PairingError::Storage(e.to_string()))?;
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .map_err(|e| PairingError::Invalid(Box::leak(e.to_string().into_boxed_str())))?;
        mac.update(&claims_json);
        let sig = mac.finalize().into_bytes();
        let token = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&claims_json),
            URL_SAFE_NO_PAD.encode(sig)
        );
        crate::telemetry::inc_bootstrap_tokens_issued(profile);
        Ok(SetupCode {
            url: url.to_string(),
            bootstrap_token: token,
            expires_at,
        })
    }

    /// Verify a previously-issued token. Returns the claims on
    /// success. Constant-time compare on the HMAC. Any tampering
    /// (modified claims, wrong sig, expired) returns the appropriate
    /// `PairingError` variant.
    pub fn verify(&self, token: &str) -> Result<TokenClaims, PairingError> {
        let (claims_b64, sig_b64) = token
            .split_once('.')
            .ok_or(PairingError::Invalid("bootstrap token format"))?;
        let claims_bytes = URL_SAFE_NO_PAD
            .decode(claims_b64)
            .map_err(|_| PairingError::Invalid("bootstrap token claims b64"))?;
        let sig = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| PairingError::Invalid("bootstrap token sig b64"))?;
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .map_err(|e| PairingError::Invalid(Box::leak(e.to_string().into_boxed_str())))?;
        mac.update(&claims_bytes);
        let expected = mac.finalize().into_bytes();
        if !bool::from(sig.ct_eq(&expected)) {
            return Err(PairingError::InvalidSignature);
        }
        let claims: TokenClaims = serde_json::from_slice(&claims_bytes)
            .map_err(|_| PairingError::Invalid("bootstrap token claims json"))?;
        if claims.expires_at < Utc::now() {
            return Err(PairingError::Expired);
        }
        Ok(claims)
    }
}

/// Encoded form: `b64url(JSON({url, bootstrap_token, expires_at}))`.
/// QR-friendly: short, URL-safe, no padding.
pub fn encode_setup_code(payload: &SetupCode) -> Result<String, PairingError> {
    let json = serde_json::to_vec(payload).map_err(|e| PairingError::Storage(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

/// Inverse of [`encode_setup_code`] for the companion side.
pub fn decode_setup_code(code: &str) -> Result<SetupCode, PairingError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(code)
        .map_err(|_| PairingError::Invalid("setup-code b64"))?;
    serde_json::from_slice(&bytes).map_err(|_| PairingError::Invalid("setup-code json"))
}

/// Convenience: returns the `expires_at` as a UTC timestamp.
pub fn token_expires_at(token: &str) -> Option<DateTime<Utc>> {
    let claims_b64 = token.split_once('.')?.0;
    let claims_bytes = URL_SAFE_NO_PAD.decode(claims_b64).ok()?;
    let claims: TokenClaims = serde_json::from_slice(&claims_bytes).ok()?;
    Some(claims.expires_at)
}
