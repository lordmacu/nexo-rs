//! Typed OAuth bundle persisted to disk by the wizard / admin RPC
//! and consumed by the LLM client at runtime.
//!
//! The on-disk shape is intentionally identical across providers so
//! `crates/llm/src/anthropic_auth.rs::OAuthState::load` (and a future
//! MiniMax equivalent) can read both with the same parser.

use serde::{Deserialize, Serialize};

/// On-disk OAuth bundle. Persisted as pretty-printed JSON via
/// [`save_atomic`] so an operator can inspect it; the daemon
/// re-reads + auto-refreshes at runtime.
///
/// The `source` field discriminates how the bundle was acquired so
/// `agent setup doctor` can surface a useful diagnosis when refresh
/// fails (e.g. "imported from claude-cli — re-run `claude login`").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OAuthBundle {
    /// Bearer token sent in `Authorization: Bearer <token>` headers.
    pub access_token: String,
    /// Long-lived refresh token used to mint new access tokens.
    pub refresh_token: String,
    /// Unix-seconds timestamp when the access token stops working.
    pub expires_at: i64,
    /// Optional account email surfaced by the provider (Anthropic
    /// returns this; MiniMax does not).
    #[serde(default)]
    pub account_email: Option<String>,
    /// RFC 3339 timestamp this bundle was first persisted.
    #[serde(default)]
    pub obtained_at: Option<String>,
    /// Origin tag — useful for `doctor` diagnostics. Free-form so
    /// new sources can land additively.
    #[serde(default)]
    pub source: Option<String>,
    /// Provider id this bundle belongs to (`anthropic`, `minimax`).
    /// Optional for back-compat with bundles persisted before this
    /// field existed; consumers fall back to the file's directory
    /// or the operator-supplied factory_type.
    #[serde(default)]
    pub provider: Option<String>,
}

impl OAuthBundle {
    /// Build a fresh bundle from an OAuth exchange/poll response.
    /// Stamps `obtained_at` with `chrono::Utc::now()` and sets
    /// `source` to the supplied tag.
    pub fn new(
        access_token: String,
        refresh_token: String,
        expires_at: i64,
        account_email: Option<String>,
        source: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        Self {
            access_token,
            refresh_token,
            expires_at,
            account_email,
            obtained_at: Some(chrono::Utc::now().to_rfc3339()),
            source: Some(source.into()),
            provider: Some(provider.into()),
        }
    }

    /// Serialize to pretty JSON for on-disk persistence.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Defensive shape check before persistence: every required
    /// field non-empty + `expires_at` looks like a unix-seconds
    /// timestamp (not milliseconds, not zero, not negative).
    pub fn validate(&self) -> Result<(), BundleValidationError> {
        if self.access_token.trim().is_empty() {
            return Err(BundleValidationError::EmptyField("access_token"));
        }
        if self.refresh_token.trim().is_empty() {
            return Err(BundleValidationError::EmptyField("refresh_token"));
        }
        if self.expires_at <= 0 {
            return Err(BundleValidationError::InvalidExpiresAt {
                value: self.expires_at,
                hint: "expires_at must be positive unix-seconds",
            });
        }
        // 10^11 ≈ year 5138 in seconds, but ≈ year 1973 in ms.
        // Anything above ⇒ caller probably handed us milliseconds.
        if self.expires_at > 100_000_000_000 {
            return Err(BundleValidationError::InvalidExpiresAt {
                value: self.expires_at,
                hint: "expires_at looks like milliseconds — divide by 1000",
            });
        }
        Ok(())
    }
}

/// Errors produced by [`OAuthBundle::validate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BundleValidationError {
    /// A required string field was empty.
    #[error("bundle missing required field `{0}`")]
    EmptyField(&'static str),
    /// `expires_at` is not a sane unix-seconds timestamp.
    #[error("invalid expires_at={value}: {hint}")]
    InvalidExpiresAt {
        /// The offending value as supplied.
        value: i64,
        /// Operator-facing hint about what shape was expected.
        hint: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_well_formed_bundle() {
        let b = OAuthBundle::new(
            "access".into(),
            "refresh".into(),
            1_700_000_000,
            Some("user@example.com".into()),
            "oauth_login",
            "anthropic",
        );
        b.validate().unwrap();
    }

    #[test]
    fn validate_rejects_empty_access_token() {
        let b = OAuthBundle::new(
            String::new(),
            "r".into(),
            1_700_000_000,
            None,
            "oauth_login",
            "anthropic",
        );
        assert!(matches!(
            b.validate(),
            Err(BundleValidationError::EmptyField("access_token"))
        ));
    }

    #[test]
    fn validate_rejects_negative_expires_at() {
        let b = OAuthBundle::new(
            "a".into(),
            "r".into(),
            -1,
            None,
            "oauth_login",
            "anthropic",
        );
        assert!(matches!(
            b.validate(),
            Err(BundleValidationError::InvalidExpiresAt { value: -1, .. })
        ));
    }

    #[test]
    fn validate_rejects_milliseconds_lookalike() {
        // 1714776600000 ms → ~2024-05-04. As seconds it would be year 56_309.
        let b = OAuthBundle::new(
            "a".into(),
            "r".into(),
            1_714_776_600_000,
            None,
            "oauth_login",
            "anthropic",
        );
        assert!(matches!(
            b.validate(),
            Err(BundleValidationError::InvalidExpiresAt { .. })
        ));
    }

    #[test]
    fn json_roundtrip_preserves_all_fields() {
        let b = OAuthBundle::new(
            "access".into(),
            "refresh".into(),
            1_700_000_000,
            Some("user@example.com".into()),
            "oauth_login",
            "anthropic",
        );
        let json = b.to_json_pretty().unwrap();
        let back: OAuthBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(back.access_token, "access");
        assert_eq!(back.refresh_token, "refresh");
        assert_eq!(back.expires_at, 1_700_000_000);
        assert_eq!(back.account_email.as_deref(), Some("user@example.com"));
        assert_eq!(back.source.as_deref(), Some("oauth_login"));
        assert_eq!(back.provider.as_deref(), Some("anthropic"));
        assert!(back.obtained_at.is_some());
    }

    #[test]
    fn json_deserialize_tolerates_legacy_bundle_without_provider() {
        // Bundles persisted before the `provider` field existed.
        let legacy = r#"{
            "access_token": "a",
            "refresh_token": "r",
            "expires_at": 1700000000
        }"#;
        let b: OAuthBundle = serde_json::from_str(legacy).unwrap();
        assert_eq!(b.access_token, "a");
        assert!(b.provider.is_none());
    }
}
