//! Phase 76.3 — `StaticTokenAuthenticator`. Equivalent to the
//! 76.1 `auth_token` field but routed through the new trait.
//! Token is held in a `Zeroizing<String>` so the underlying
//! buffer is wiped at drop; comparison runs in constant time via
//! `subtle`.

use async_trait::async_trait;
use axum::http::HeaderMap;
use zeroize::Zeroizing;

use super::{
    consteq_bytes, principal::AuthMethod, tenant::TenantId, AuthRejection, AuthRejectionReason,
    McpAuthenticator, Principal,
};

pub struct StaticTokenAuthenticator {
    expected: Zeroizing<String>,
    tenant: TenantId,
}

impl StaticTokenAuthenticator {
    /// Phase 76.3 entry point — keeps the old shape (no tenant override).
    pub fn new(token: String) -> Self {
        Self {
            expected: Zeroizing::new(token),
            tenant: TenantId::parse(TenantId::DEFAULT).expect("DEFAULT is a valid TenantId"),
        }
    }

    /// Phase 76.4 entry point — operator may pin the tenant for this
    /// token via YAML `auth.tenant: ...`.
    pub fn with_tenant(token: String, tenant: TenantId) -> Self {
        Self {
            expected: Zeroizing::new(token),
            tenant,
        }
    }
}

#[async_trait]
impl McpAuthenticator for StaticTokenAuthenticator {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        _bind: std::net::SocketAddr,
    ) -> Result<Principal, AuthRejection> {
        let provided = extract_bearer(headers).ok_or_else(|| AuthRejection {
            reason: AuthRejectionReason::Missing,
            detail: "no Authorization Bearer or Mcp-Auth-Token header".into(),
        })?;
        if !consteq_bytes(provided.as_bytes(), self.expected.as_bytes()) {
            return Err(AuthRejection {
                reason: AuthRejectionReason::SignatureInvalid,
                detail: "static token mismatch".into(),
            });
        }
        Ok(Principal {
            tenant: self.tenant.clone(),
            subject: "static-token-holder".into(),
            scopes: Vec::new(),
            auth_method: AuthMethod::StaticToken,
            claims: Default::default(),
        })
    }

    fn label(&self) -> &'static str {
        "static_token"
    }
}

/// Extract the bearer token from either:
///   * `Authorization: Bearer X` (RFC 6750 standard)
///   * `Mcp-Auth-Token: X` (legacy, kept for stdio→HTTP parity)
///
/// Returns `None` when neither header carries a non-empty value.
pub(super) fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    if let Some(s) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string())
    {
        if !s.is_empty() {
            return Some(s);
        }
    }
    headers
        .get("mcp-auth-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn auth() -> StaticTokenAuthenticator {
        StaticTokenAuthenticator::new("secret-token".into())
    }

    fn bind() -> std::net::SocketAddr {
        "127.0.0.1:7575".parse().unwrap()
    }

    #[tokio::test]
    async fn bearer_header_recognized() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        let p = auth().authenticate(&h, bind()).await.unwrap();
        assert_eq!(p.auth_method, AuthMethod::StaticToken);
        assert_eq!(p.subject, "static-token-holder");
    }

    #[tokio::test]
    async fn legacy_header_recognized() {
        let mut h = HeaderMap::new();
        h.insert("mcp-auth-token", HeaderValue::from_static("secret-token"));
        let p = auth().authenticate(&h, bind()).await.unwrap();
        assert_eq!(p.auth_method, AuthMethod::StaticToken);
    }

    #[tokio::test]
    async fn missing_header_rejected_with_missing_reason() {
        let h = HeaderMap::new();
        let err = auth().authenticate(&h, bind()).await.unwrap_err();
        assert_eq!(err.reason, AuthRejectionReason::Missing);
    }

    #[tokio::test]
    async fn wrong_token_rejected_with_signature_invalid_reason() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong-token"),
        );
        let err = auth().authenticate(&h, bind()).await.unwrap_err();
        assert_eq!(err.reason, AuthRejectionReason::SignatureInvalid);
    }

    #[tokio::test]
    async fn correct_token_returns_principal_with_static_token_method() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        let p = auth().authenticate(&h, bind()).await.unwrap();
        assert_eq!(p.auth_method, AuthMethod::StaticToken);
        assert_eq!(p.tenant.as_str(), TenantId::DEFAULT);
        assert!(p.scopes.is_empty());
    }
}
