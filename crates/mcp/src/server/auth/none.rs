//! Phase 76.3 — `NoneAuthenticator`. Dev-only loopback bypass; the
//! constructor refuses non-loopback binds so a misconfigured public
//! deployment never accepts unauthenticated traffic.

use async_trait::async_trait;
use axum::http::HeaderMap;

use super::{principal::AuthMethod, AuthRejection, McpAuthenticator, Principal};
use crate::server::http_config::is_loopback;

#[derive(Debug)]
pub struct NoneAuthenticator;

impl NoneAuthenticator {
    /// Boot fails when the bind is non-loopback. There is no
    /// runtime escape hatch — operators wanting unauthenticated
    /// access on a non-loopback bind must also (a) use a reverse
    /// proxy that performs auth and (b) keep the agent on
    /// loopback. 76.13 will introduce more nuanced TLS modes.
    pub fn new(bind: std::net::SocketAddr) -> Result<Self, String> {
        if !is_loopback(&bind.ip()) {
            return Err(format!(
                "auth: none requires loopback bind; got {bind} \
                 (production deployments must use static_token, bearer_jwt, or mutual_tls)"
            ));
        }
        Ok(Self)
    }
}

#[async_trait]
impl McpAuthenticator for NoneAuthenticator {
    async fn authenticate(
        &self,
        _headers: &HeaderMap,
        _bind: std::net::SocketAddr,
    ) -> Result<Principal, AuthRejection> {
        let mut p = Principal::anonymous();
        p.auth_method = AuthMethod::None;
        Ok(p)
    }

    fn label(&self) -> &'static str {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn boot_refuses_non_loopback() {
        let bind: std::net::SocketAddr = "0.0.0.0:7575".parse().unwrap();
        let res = NoneAuthenticator::new(bind);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("loopback"));
    }

    #[tokio::test]
    async fn loopback_returns_anonymous_principal() {
        let bind: std::net::SocketAddr = "127.0.0.1:7575".parse().unwrap();
        let auth = NoneAuthenticator::new(bind).unwrap();
        let p = auth.authenticate(&HeaderMap::new(), bind).await.unwrap();
        assert_eq!(p.subject, "anonymous");
        assert_eq!(p.auth_method, AuthMethod::None);
    }

    #[tokio::test]
    async fn label_matches_yaml_kind() {
        let bind: std::net::SocketAddr = "127.0.0.1:7575".parse().unwrap();
        let auth = NoneAuthenticator::new(bind).unwrap();
        assert_eq!(auth.label(), "none");
    }
}
