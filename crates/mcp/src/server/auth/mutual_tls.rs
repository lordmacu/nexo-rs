//! Phase 76.3 — `MutualTlsAuthenticator`. Currently supports the
//! `FromHeader` source — the cert identity is forwarded by a trusted
//! reverse proxy (nginx `ssl_client_s_dn`, Envoy
//! `x-forwarded-client-cert`, Caddy `tls.client.subject.cn`, …).
//!
//! Boot validation forces a loopback bind so a misconfigured public
//! deployment cannot accept spoofed certificate headers from
//! arbitrary attackers. 76.13 will introduce a `Native` variant
//! that reads the peer certificate from an in-process rustls
//! TLS layer; the enum is `#[non_exhaustive]` to make that
//! addition non-breaking.

use std::collections::{BTreeMap, HashSet};

use async_trait::async_trait;
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use super::{
    principal::AuthMethod, tenant::TenantId, AuthRejection, AuthRejectionReason, McpAuthenticator,
    Principal,
};
use crate::server::http_config::is_loopback;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum MutualTlsConfig {
    /// Certificate identity forwarded as a header by a reverse
    /// proxy. Operator MUST keep the agent on a loopback bind so
    /// arbitrary clients cannot spoof the header value.
    FromHeader {
        #[serde(default = "default_header_name")]
        header_name: String,
        /// Allowlisted CN / SAN values (exact match — no glob, no
        /// substring).
        cn_allowlist: Vec<String>,
        /// Phase 76.4 — optional CN → tenant remap. When absent,
        /// the CN itself is parsed as a [`TenantId`] (a CN that
        /// can't pass the strict validator — e.g. one with `.` —
        /// is rejected with `TenantClaimMissing` so the operator
        /// is forced to either rename the CN or supply the map).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cn_to_tenant: Option<BTreeMap<String, String>>,
    },
}

fn default_header_name() -> String {
    "x-client-cert-cn".into()
}

#[derive(Debug)]
pub struct MutualTlsAuthenticator {
    cfg: MutualTlsConfig,
}

impl MutualTlsAuthenticator {
    pub fn new(cfg: MutualTlsConfig, bind: std::net::SocketAddr) -> Result<Self, String> {
        match &cfg {
            MutualTlsConfig::FromHeader {
                cn_allowlist,
                header_name,
                cn_to_tenant,
            } => {
                if !is_loopback(&bind.ip()) {
                    return Err(format!(
                        "mutual_tls.from_header requires a loopback bind \
                         (terminate TLS at the reverse proxy and forward to 127.0.0.1); \
                         got {bind}"
                    ));
                }
                if cn_allowlist.is_empty() {
                    return Err("mutual_tls.cn_allowlist must not be empty".into());
                }
                if header_name.trim().is_empty() {
                    return Err("mutual_tls.header_name must not be empty".into());
                }
                // Boot-validate the cn_to_tenant map values — every
                // value must be a valid TenantId. Catches operator
                // typos (e.g. `Tenant-A`) before any client connects.
                if let Some(map) = cn_to_tenant {
                    for (cn, tenant_str) in map {
                        TenantId::parse(tenant_str).map_err(|e| {
                            format!(
                                "mutual_tls.cn_to_tenant['{cn}'] = '{tenant_str}' is not a valid tenant id: {e}"
                            )
                        })?;
                    }
                }
            }
        }
        Ok(Self { cfg })
    }
}

#[async_trait]
impl McpAuthenticator for MutualTlsAuthenticator {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        _bind: std::net::SocketAddr,
    ) -> Result<Principal, AuthRejection> {
        match &self.cfg {
            MutualTlsConfig::FromHeader {
                header_name,
                cn_allowlist,
                cn_to_tenant,
            } => {
                let provided = headers
                    .get(header_name.as_str())
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| AuthRejection {
                        reason: AuthRejectionReason::Missing,
                        detail: format!("missing {header_name} header"),
                    })?;
                let allow: HashSet<&str> = cn_allowlist.iter().map(|s| s.as_str()).collect();
                if !allow.contains(provided.as_str()) {
                    return Err(AuthRejection {
                        reason: AuthRejectionReason::CnNotAllowed,
                        detail: format!("cn `{provided}` not allowlisted"),
                    });
                }
                // Phase 76.4 — derive tenant: prefer the remap, fall
                // back to parsing the CN itself. CNs that don't pass
                // the strict TenantId validator (e.g. contain a dot)
                // require a remap or are rejected.
                let tenant = if let Some(map) = cn_to_tenant {
                    if let Some(t) = map.get(&provided) {
                        TenantId::parse(t)
                            .expect("boot validation guarantees cn_to_tenant values parse")
                    } else {
                        TenantId::parse(&provided).map_err(|e| AuthRejection {
                            reason: AuthRejectionReason::TenantClaimMissing,
                            detail: format!(
                                "cn `{provided}` not in cn_to_tenant map and not a valid tenant id: {e}"
                            ),
                        })?
                    }
                } else {
                    TenantId::parse(&provided).map_err(|e| AuthRejection {
                        reason: AuthRejectionReason::TenantClaimMissing,
                        detail: format!(
                            "cn `{provided}` is not a valid tenant id (consider cn_to_tenant remap): {e}"
                        ),
                    })?
                };
                Ok(Principal {
                    tenant,
                    subject: provided,
                    scopes: Vec::new(),
                    auth_method: AuthMethod::MutualTls,
                    claims: Default::default(),
                })
            }
        }
    }

    fn label(&self) -> &'static str {
        "mutual_tls"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn loopback() -> std::net::SocketAddr {
        "127.0.0.1:7575".parse().unwrap()
    }

    #[tokio::test]
    async fn boot_refuses_non_loopback_bind() {
        let cfg = MutualTlsConfig::FromHeader {
            header_name: default_header_name(),
            cn_allowlist: vec!["client-1".into()],
            cn_to_tenant: None,
        };
        let bind: std::net::SocketAddr = "0.0.0.0:7575".parse().unwrap();
        let res = MutualTlsAuthenticator::new(cfg, bind);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("loopback"));
    }

    #[tokio::test]
    async fn boot_refuses_empty_allowlist() {
        let cfg = MutualTlsConfig::FromHeader {
            header_name: default_header_name(),
            cn_allowlist: vec![],
            cn_to_tenant: None,
        };
        let res = MutualTlsAuthenticator::new(cfg, loopback());
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("allowlist"));
    }

    #[tokio::test]
    async fn cn_allowed_returns_principal() {
        let cfg = MutualTlsConfig::FromHeader {
            header_name: default_header_name(),
            cn_allowlist: vec!["client-1".into()],
            cn_to_tenant: None,
        };
        let auth = MutualTlsAuthenticator::new(cfg, loopback()).unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-client-cert-cn", HeaderValue::from_static("client-1"));
        let p = auth.authenticate(&h, loopback()).await.unwrap();
        assert_eq!(p.subject, "client-1");
        assert_eq!(p.auth_method, AuthMethod::MutualTls);
    }

    #[tokio::test]
    async fn cn_not_allowed_rejected() {
        let cfg = MutualTlsConfig::FromHeader {
            header_name: default_header_name(),
            cn_allowlist: vec!["client-1".into()],
            cn_to_tenant: None,
        };
        let auth = MutualTlsAuthenticator::new(cfg, loopback()).unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-client-cert-cn", HeaderValue::from_static("evil"));
        let err = auth.authenticate(&h, loopback()).await.unwrap_err();
        assert_eq!(err.reason, AuthRejectionReason::CnNotAllowed);
    }

    #[tokio::test]
    async fn missing_header_rejected_missing() {
        let cfg = MutualTlsConfig::FromHeader {
            header_name: default_header_name(),
            cn_allowlist: vec!["client-1".into()],
            cn_to_tenant: None,
        };
        let auth = MutualTlsAuthenticator::new(cfg, loopback()).unwrap();
        let h = HeaderMap::new();
        let err = auth.authenticate(&h, loopback()).await.unwrap_err();
        assert_eq!(err.reason, AuthRejectionReason::Missing);
    }
}
