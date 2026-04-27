//! Phase 76.3 — `BearerJwtAuthenticator`. Validates incoming JWTs
//! against a JWKS document with rotation, single-flight refresh
//! and stale-OK fallback (see `jwks_cache.rs`).
//!
//! Boot validation (in `BearerJwtAuthenticator::new`):
//!   * algorithm allowlist non-empty;
//!   * `none` rejected explicitly;
//!   * mixing HS* and asymmetric families rejected (algorithm
//!     confusion CVE class).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::HeaderMap;
use jsonwebtoken::{Algorithm, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    jwks_cache::{JwksCache, JwksError},
    principal::AuthMethod,
    static_token::extract_bearer,
    AuthRejection, AuthRejectionReason, McpAuthenticator, Principal,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JwtConfig {
    pub jwks_url: String,
    #[serde(default = "default_jwks_ttl")]
    pub jwks_cache_ttl_secs: u64,
    #[serde(default = "default_jwks_cooldown")]
    pub jwks_refresh_cooldown_secs: u64,
    /// Algorithm allowlist. Must contain only RS*/PS*/ES*/EdDSA
    /// family OR HS* family — never both (algorithm-confusion
    /// defense).
    #[serde(default = "default_algorithms")]
    pub algorithms: Vec<String>,
    pub issuer: String,
    pub audiences: Vec<String>,
    #[serde(default = "default_tenant_claim")]
    pub tenant_claim: String,
    #[serde(default = "default_scopes_claim")]
    pub scopes_claim: String,
    #[serde(default = "default_leeway")]
    pub leeway_secs: u64,
}

fn default_jwks_ttl() -> u64 {
    600
}
fn default_jwks_cooldown() -> u64 {
    60
}
fn default_algorithms() -> Vec<String> {
    vec!["RS256".into()]
}
fn default_tenant_claim() -> String {
    "tenant_id".into()
}
fn default_scopes_claim() -> String {
    "scope".into()
}
fn default_leeway() -> u64 {
    60
}

pub struct BearerJwtAuthenticator {
    cfg: JwtConfig,
    validation: Validation,
    jwks: Arc<JwksCache>,
}

impl std::fmt::Debug for BearerJwtAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BearerJwtAuthenticator")
            .field("issuer", &self.cfg.issuer)
            .field("audiences", &self.cfg.audiences)
            .field("algorithms", &self.cfg.algorithms)
            .finish()
    }
}

impl BearerJwtAuthenticator {
    pub fn new(cfg: JwtConfig) -> Result<Self, String> {
        if cfg.algorithms.is_empty() {
            return Err("bearer_jwt.algorithms must not be empty".into());
        }
        if cfg
            .algorithms
            .iter()
            .any(|s| s.eq_ignore_ascii_case("none"))
        {
            return Err("bearer_jwt: alg `none` is forbidden".into());
        }
        let mut algs = Vec::with_capacity(cfg.algorithms.len());
        for s in &cfg.algorithms {
            let a = match s.as_str() {
                "HS256" => Algorithm::HS256,
                "HS384" => Algorithm::HS384,
                "HS512" => Algorithm::HS512,
                "RS256" => Algorithm::RS256,
                "RS384" => Algorithm::RS384,
                "RS512" => Algorithm::RS512,
                "PS256" => Algorithm::PS256,
                "PS384" => Algorithm::PS384,
                "PS512" => Algorithm::PS512,
                "ES256" => Algorithm::ES256,
                "ES384" => Algorithm::ES384,
                "EdDSA" => Algorithm::EdDSA,
                other => return Err(format!("bearer_jwt: unknown algorithm `{other}`")),
            };
            algs.push(a);
        }
        // Algorithm-confusion defense: HS* and asymmetric must not
        // coexist in the allowlist (CVE-2018-1000531 class).
        let has_hs = algs
            .iter()
            .any(|a| matches!(a, Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512));
        let has_asym = algs
            .iter()
            .any(|a| !matches!(a, Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512));
        if has_hs && has_asym {
            return Err(
                "bearer_jwt: algorithms must not mix HS* and asymmetric families \
                 (algorithm-confusion attack)"
                    .into(),
            );
        }

        let mut validation = Validation::new(algs[0]);
        validation.algorithms = algs.clone();
        validation.set_issuer(&[cfg.issuer.as_str()]);
        validation.set_audience(&cfg.audiences);
        validation.leeway = cfg.leeway_secs;
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.validate_aud = true;

        let jwks = Arc::new(JwksCache::new(
            cfg.jwks_url.clone(),
            Duration::from_secs(cfg.jwks_cache_ttl_secs),
            Duration::from_secs(cfg.jwks_refresh_cooldown_secs),
        ));

        Ok(Self {
            cfg,
            validation,
            jwks,
        })
    }
}

#[async_trait]
impl McpAuthenticator for BearerJwtAuthenticator {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        _bind: std::net::SocketAddr,
    ) -> Result<Principal, AuthRejection> {
        let token = extract_bearer(headers).ok_or_else(|| AuthRejection {
            reason: AuthRejectionReason::Missing,
            detail: "no Authorization Bearer".into(),
        })?;

        let header = jsonwebtoken::decode_header(&token).map_err(|e| AuthRejection {
            reason: AuthRejectionReason::Malformed,
            detail: format!("bad jwt header: {e}"),
        })?;

        if !self.validation.algorithms.contains(&header.alg) {
            return Err(AuthRejection {
                reason: AuthRejectionReason::AlgorithmDisallowed,
                detail: format!("alg {:?} not in allowlist", header.alg),
            });
        }

        let kid = header.kid.ok_or_else(|| AuthRejection {
            reason: AuthRejectionReason::Malformed,
            detail: "jwt missing `kid`".into(),
        })?;

        let key = self.jwks.get_or_refresh(&kid).await.map_err(|e| match e {
            JwksError::Unknown => AuthRejection {
                reason: AuthRejectionReason::UnknownKid,
                detail: format!("kid `{kid}` not found"),
            },
            JwksError::Unreachable(d) => AuthRejection {
                reason: AuthRejectionReason::JwksUnreachable,
                detail: d,
            },
        })?;

        let data = jsonwebtoken::decode::<Value>(&token, &key, &self.validation)
            .map_err(classify_jwt_error)?;

        let claims_obj = data.claims.as_object().cloned().unwrap_or_default();
        let subject = claims_obj
            .get("sub")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Phase 76.4 — tenant claim is mandatory and must parse
        // through the strict `TenantId` validator. Missing or
        // malformed → uniform 401 (`TenantClaimMissing`). The
        // detail goes only to `tracing::warn!`.
        let tenant_raw = claims_obj
            .get(&self.cfg.tenant_claim)
            .and_then(|v| v.as_str())
            .ok_or_else(|| AuthRejection {
                reason: AuthRejectionReason::TenantClaimMissing,
                detail: format!("claim `{}` missing or non-string", self.cfg.tenant_claim),
            })?;
        let tenant = super::tenant::TenantId::parse(tenant_raw).map_err(|e| AuthRejection {
            reason: AuthRejectionReason::TenantClaimMissing,
            detail: format!("claim `{}` invalid: {e}", self.cfg.tenant_claim),
        })?;
        let scopes = parse_scopes(claims_obj.get(&self.cfg.scopes_claim));
        let claims = claims_obj.into_iter().collect();

        Ok(Principal {
            tenant,
            subject,
            scopes,
            auth_method: AuthMethod::Jwt,
            claims,
        })
    }

    fn label(&self) -> &'static str {
        "bearer_jwt"
    }
}

pub(super) fn classify_jwt_error(e: jsonwebtoken::errors::Error) -> AuthRejection {
    use jsonwebtoken::errors::ErrorKind::*;
    match e.kind() {
        ExpiredSignature => AuthRejection {
            reason: AuthRejectionReason::Expired,
            detail: e.to_string(),
        },
        ImmatureSignature => AuthRejection {
            reason: AuthRejectionReason::NotYetValid,
            detail: e.to_string(),
        },
        InvalidAudience => AuthRejection {
            reason: AuthRejectionReason::AudienceMismatch,
            detail: e.to_string(),
        },
        InvalidIssuer => AuthRejection {
            reason: AuthRejectionReason::IssuerMismatch,
            detail: e.to_string(),
        },
        InvalidSignature | InvalidEcdsaKey | InvalidRsaKey(_) => AuthRejection {
            reason: AuthRejectionReason::SignatureInvalid,
            detail: e.to_string(),
        },
        InvalidAlgorithm => AuthRejection {
            reason: AuthRejectionReason::AlgorithmDisallowed,
            detail: e.to_string(),
        },
        _ => AuthRejection {
            reason: AuthRejectionReason::Malformed,
            detail: e.to_string(),
        },
    }
}

pub(super) fn parse_scopes(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => s.split_whitespace().map(String::from).collect(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> JwtConfig {
        JwtConfig {
            jwks_url: "http://127.0.0.1:1/jwks.json".into(),
            jwks_cache_ttl_secs: 600,
            jwks_refresh_cooldown_secs: 60,
            algorithms: vec!["RS256".into()],
            issuer: "https://idp.example/".into(),
            audiences: vec!["nexo-mcp".into()],
            tenant_claim: "tenant_id".into(),
            scopes_claim: "scope".into(),
            leeway_secs: 60,
        }
    }

    #[test]
    fn boot_refuses_alg_none() {
        let mut c = cfg();
        c.algorithms = vec!["none".into()];
        assert!(BearerJwtAuthenticator::new(c).is_err());
    }

    #[test]
    fn boot_refuses_empty_algorithms() {
        let mut c = cfg();
        c.algorithms = vec![];
        assert!(BearerJwtAuthenticator::new(c).is_err());
    }

    #[test]
    fn boot_refuses_alg_confusion_rs_hs_mix() {
        let mut c = cfg();
        c.algorithms = vec!["RS256".into(), "HS256".into()];
        let err = BearerJwtAuthenticator::new(c).unwrap_err();
        assert!(err.contains("confusion"));
    }

    #[test]
    fn boot_accepts_pure_hs_or_pure_asymmetric() {
        let mut c = cfg();
        c.algorithms = vec!["HS256".into(), "HS512".into()];
        assert!(BearerJwtAuthenticator::new(c).is_ok());
        let mut c = cfg();
        c.algorithms = vec!["RS256".into(), "ES256".into()];
        assert!(BearerJwtAuthenticator::new(c).is_ok());
    }

    #[test]
    fn parse_scopes_handles_string_and_array() {
        assert_eq!(
            parse_scopes(Some(&Value::String("read write".into()))),
            vec!["read".to_string(), "write".to_string()]
        );
        assert_eq!(
            parse_scopes(Some(&serde_json::json!(["a", "b"]))),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(parse_scopes(None), Vec::<String>::new());
    }

    #[test]
    fn classify_jwt_error_maps_expected_kinds() {
        use jsonwebtoken::errors::{Error, ErrorKind};
        let e: Error = ErrorKind::ExpiredSignature.into();
        assert_eq!(classify_jwt_error(e).reason, AuthRejectionReason::Expired);
        let e: Error = ErrorKind::InvalidAudience.into();
        assert_eq!(
            classify_jwt_error(e).reason,
            AuthRejectionReason::AudienceMismatch
        );
        let e: Error = ErrorKind::InvalidIssuer.into();
        assert_eq!(
            classify_jwt_error(e).reason,
            AuthRejectionReason::IssuerMismatch
        );
    }
}
