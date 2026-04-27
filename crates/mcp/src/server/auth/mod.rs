//! Phase 76.3 — pluggable authentication for the MCP HTTP transport.
//!
//! The trait [`McpAuthenticator`] is what the HTTP handlers call.
//! Three production impls ship in this sub-phase:
//!   * [`static_token::StaticTokenAuthenticator`] — bearer token,
//!     constant-time compared via `subtle`.
//!   * [`bearer_jwt::BearerJwtAuthenticator`] — JWT validated against
//!     a JWKS document with rotation, single-flight refresh and
//!     stale-OK fallback.
//!   * [`mutual_tls::MutualTlsAuthenticator`] — peer-cert identity
//!     forwarded by a trusted reverse proxy (76.13 will add native
//!     in-process rustls).
//!
//! Plus a [`none::NoneAuthenticator`] for dev/loopback work that
//! refuses to construct on a non-loopback bind.
//!
//! All rejection paths converge on [`AuthRejection`] and produce
//! identical 401 bodies (anti-enumeration); the structured reason is
//! only logged at `warn` for the operator.

pub mod bearer_jwt;
pub mod jwks_cache;
pub mod mutual_tls;
pub mod none;
pub mod principal;
pub mod static_token;
pub mod tenant;

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use principal::{AuthMethod, Principal};
pub use tenant::{
    tenant_db_path, tenant_scoped_canonicalize, tenant_scoped_path, CrossTenantError, TenantId,
    TenantIdError, TenantPathError, TenantScoped,
};

/// Async authentication contract. Implementations must be cheap to
/// clone (`Arc` internally) since axum routes hold state across
/// concurrent requests.
#[async_trait]
pub trait McpAuthenticator: Send + Sync {
    /// Authenticate based on request headers. The bind address is
    /// passed so impls can double-check the boot-time loopback
    /// assumption when needed (defence in depth against later
    /// reconfiguration drift).
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        bind: std::net::SocketAddr,
    ) -> Result<Principal, AuthRejection>;

    /// Label used for tracing / metrics. Conventionally lowercase
    /// snake_case matching the YAML `kind` discriminator.
    fn label(&self) -> &'static str {
        "unknown"
    }
}

#[derive(Debug, Error, Clone)]
#[error("auth rejection: {reason:?}: {detail}")]
pub struct AuthRejection {
    pub reason: AuthRejectionReason,
    /// Detail intended for `tracing::warn!` only — never returned
    /// on the wire (anti-enumeration).
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRejectionReason {
    Missing,
    Malformed,
    Expired,
    NotYetValid,
    SignatureInvalid,
    AlgorithmDisallowed,
    AudienceMismatch,
    IssuerMismatch,
    UnknownKid,
    /// JWKS endpoint unreachable AND no usable cached key — maps
    /// to HTTP 503 (not 401), since the failure is on our side.
    JwksUnreachable,
    TenantClaimMissing,
    CnNotAllowed,
}

impl AuthRejection {
    pub fn missing(detail: impl Into<String>) -> Self {
        Self {
            reason: AuthRejectionReason::Missing,
            detail: detail.into(),
        }
    }
    pub fn malformed(detail: impl Into<String>) -> Self {
        Self {
            reason: AuthRejectionReason::Malformed,
            detail: detail.into(),
        }
    }
    pub fn signature_invalid(detail: impl Into<String>) -> Self {
        Self {
            reason: AuthRejectionReason::SignatureInvalid,
            detail: detail.into(),
        }
    }
    pub fn cn_not_allowed(detail: impl Into<String>) -> Self {
        Self {
            reason: AuthRejectionReason::CnNotAllowed,
            detail: detail.into(),
        }
    }

    /// Map to HTTP response. ALL rejections except `JwksUnreachable`
    /// produce identical 401 body to defeat enumeration; the reason
    /// only goes to `tracing::warn!`.
    pub fn into_http(self) -> Response {
        if matches!(self.reason, AuthRejectionReason::JwksUnreachable) {
            tracing::warn!(
                reason = ?self.reason,
                detail = %self.detail,
                "mcp auth: jwks unreachable"
            );
            let body = serde_json::json!({
                "jsonrpc":"2.0",
                "error":{"code":-32099,"message":"authentication backend unavailable"},
                "id":Value::Null,
            });
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [("content-type", "application/json"), ("retry-after", "5")],
                body.to_string(),
            )
                .into_response();
        }
        tracing::warn!(reason = ?self.reason, detail = %self.detail, "mcp auth: rejected");
        let body = serde_json::json!({
            "jsonrpc":"2.0",
            "error":{"code":-32001,"message":"unauthorized"},
            "id":Value::Null,
        });
        (
            StatusCode::UNAUTHORIZED,
            [("content-type", "application/json")],
            body.to_string(),
        )
            .into_response()
    }
}

/// YAML-facing config; mirrors the runtime authenticator factory.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthConfig {
    None,
    StaticToken {
        /// Resolved token value (env var resolved by the YAML layer
        /// before this struct sees it).
        #[serde(skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        /// Env var name for callers to resolve at YAML-load time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_env: Option<String>,
        /// Phase 76.4 — operator-pinned tenant for this token. When
        /// absent, the principal carries [`TenantId::DEFAULT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tenant: Option<String>,
    },
    BearerJwt(bearer_jwt::JwtConfig),
    MutualTls(mutual_tls::MutualTlsConfig),
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self::None
    }
}

impl AuthConfig {
    /// Build the runtime authenticator. Each impl performs its own
    /// boot validation (algorithm allowlist for JWT,
    /// loopback-only for mTLS-from-header / none).
    pub fn build(&self, bind: std::net::SocketAddr) -> Result<Arc<dyn McpAuthenticator>, String> {
        match self {
            Self::None => {
                Ok(Arc::new(none::NoneAuthenticator::new(bind)?) as Arc<dyn McpAuthenticator>)
            }
            Self::StaticToken {
                token,
                token_env,
                tenant,
            } => {
                let resolved = match (token.as_deref(), token_env.as_deref()) {
                    (Some(t), _) if !t.is_empty() => t.to_string(),
                    (_, Some(env_name)) => std::env::var(env_name).map_err(|_| {
                        format!("static_token: env var `{env_name}` not set or unreadable")
                    })?,
                    _ => return Err("static_token: provide either `token` or `token_env`".into()),
                };
                if resolved.trim().is_empty() {
                    return Err("static_token: resolved token is empty".into());
                }
                let tenant_id = match tenant.as_deref() {
                    Some(t) => tenant::TenantId::parse(t)
                        .map_err(|e| format!("static_token.tenant `{t}` invalid: {e}"))?,
                    None => tenant::TenantId::parse(tenant::TenantId::DEFAULT)
                        .expect("DEFAULT is a valid TenantId"),
                };
                Ok(
                    Arc::new(static_token::StaticTokenAuthenticator::with_tenant(
                        resolved, tenant_id,
                    )) as Arc<dyn McpAuthenticator>,
                )
            }
            Self::BearerJwt(cfg) => Ok(Arc::new(bearer_jwt::BearerJwtAuthenticator::new(
                cfg.clone(),
            )?) as Arc<dyn McpAuthenticator>),
            Self::MutualTls(cfg) => Ok(Arc::new(mutual_tls::MutualTlsAuthenticator::new(
                cfg.clone(),
                bind,
            )?) as Arc<dyn McpAuthenticator>),
        }
    }
}

/// Constant-time bytes equality. Returns `false` immediately on
/// length mismatch — the length channel is not protected (operator
/// chooses fixed-length tokens), but the byte comparison itself runs
/// in constant time over `min(a.len(), b.len())`.
pub fn consteq_bytes(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consteq_bytes_basic() {
        assert!(consteq_bytes(b"hello", b"hello"));
        assert!(!consteq_bytes(b"hello", b"hellp"));
        assert!(!consteq_bytes(b"hello", b"hellos"));
        assert!(consteq_bytes(b"", b""));
    }

    #[test]
    fn auth_rejection_into_http_uniform_401() {
        let r1 = AuthRejection::missing("a");
        let r2 = AuthRejection::malformed("b");
        let r3 = AuthRejection::signature_invalid("c");
        // All three render the same body and status.
        let resp1 = r1.into_http();
        let resp2 = r2.into_http();
        let resp3 = r3.into_http();
        assert_eq!(resp1.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp2.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp3.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn auth_rejection_jwks_unreachable_maps_503() {
        let r = AuthRejection {
            reason: AuthRejectionReason::JwksUnreachable,
            detail: "x".into(),
        };
        let resp = r.into_http();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn config_default_is_none() {
        let c = AuthConfig::default();
        assert!(matches!(c, AuthConfig::None));
    }

    #[test]
    fn config_yaml_static_token() {
        let yaml = r#"
kind: static_token
token: "secret"
"#;
        let c: AuthConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            c,
            AuthConfig::StaticToken { token: Some(ref t), .. } if t == "secret"
        ));
    }

    #[test]
    fn config_yaml_none() {
        let c: AuthConfig = serde_yaml::from_str("kind: none\n").unwrap();
        assert!(matches!(c, AuthConfig::None));
    }
}
