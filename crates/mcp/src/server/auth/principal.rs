//! Phase 76.3 — caller identity surfaced from HTTP / stdio auth.
//! Phase 76.4 — `tenant_id` promoted to a validated [`TenantId`].
//!
//! `Principal` is the credential-side projection of an authenticated
//! request: who is calling, which tenant they belong to, what scopes
//! they hold, and the raw claims they presented (when applicable).
//! `DispatchContext::principal` carries this struct from the
//! transport layer down to tools so 76.4 (multi-tenant) and 76.11
//! (audit log) can consume it without reaching back through axum.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::tenant::TenantId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Principal {
    /// Validated tenant identifier. Phase 76.4 makes this always
    /// present — stdio uses [`TenantId::STDIO_LOCAL`], static-token
    /// without an explicit `tenant:` block uses [`TenantId::DEFAULT`].
    pub tenant: TenantId,

    /// Stable subject identifier:
    ///   * JWT — the `sub` claim
    ///   * mTLS — the matched CN
    ///   * static token — `"static-token-holder"`
    ///   * stdio — `"local"`
    ///   * none — `"anonymous"`
    pub subject: String,

    /// RBAC-ish scopes; populated from the JWT `scope` / `scopes`
    /// claim (or its configured equivalent). Empty for non-JWT auth.
    pub scopes: Vec<String>,

    /// Discriminator used for tracing labels and per-method routing.
    pub auth_method: AuthMethod,

    /// Raw claims (or empty map for non-JWT). Useful for audit
    /// (76.11) and for downstream tools that want to mirror claims
    /// into their own structured logs.
    #[serde(default)]
    pub claims: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// Process-inherited trust — the caller is the process parent
    /// of an `agent mcp-server` stdio invocation.
    Stdio,
    /// Static bearer token configured at boot.
    StaticToken,
    /// JWT validated against a JWKS document.
    Jwt,
    /// Mutual TLS — peer certificate identity. Phase 76.3 supplies
    /// the `FromHeader` source; 76.13 will add the in-process
    /// rustls source.
    MutualTls,
    /// `NoneAuthenticator` — dev-only loopback bypass.
    None,
}

impl Principal {
    /// Identity injected by the stdio transport. Constant per
    /// process — every stdio request inherits the same shape.
    pub fn stdio_local() -> Self {
        Self {
            tenant: TenantId::parse(TenantId::STDIO_LOCAL)
                .expect("STDIO_LOCAL is a valid TenantId"),
            subject: "local".into(),
            scopes: Vec::new(),
            auth_method: AuthMethod::Stdio,
            claims: BTreeMap::new(),
        }
    }

    /// Identity emitted by `NoneAuthenticator` (dev only).
    pub fn anonymous() -> Self {
        Self {
            tenant: TenantId::parse(TenantId::STDIO_LOCAL)
                .expect("STDIO_LOCAL is a valid TenantId"),
            subject: "anonymous".into(),
            scopes: Vec::new(),
            auth_method: AuthMethod::None,
            claims: BTreeMap::new(),
        }
    }

    /// Test-only constructor — keeps the same shape as `stdio_local`
    /// but is plain on the tin. Phase 76.4 keeps this `pub(crate)` so
    /// callers can't accidentally use it as a real principal.
    pub(crate) fn test_local() -> Self {
        Self::stdio_local()
    }

    /// Backward-compat shim: `tenant_id() -> &str`.
    pub fn tenant_id(&self) -> &str {
        self.tenant.as_str()
    }

    /// True when `name` appears in `self.scopes`.
    pub fn has_scope(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_local_has_expected_shape() {
        let p = Principal::stdio_local();
        assert_eq!(p.subject, "local");
        assert_eq!(p.auth_method, AuthMethod::Stdio);
        assert_eq!(p.tenant.as_str(), TenantId::STDIO_LOCAL);
        assert!(p.scopes.is_empty());
    }

    #[test]
    fn anonymous_has_none_method() {
        let p = Principal::anonymous();
        assert_eq!(p.auth_method, AuthMethod::None);
        assert_eq!(p.subject, "anonymous");
    }

    #[test]
    fn has_scope_matches_exact() {
        let mut p = Principal::anonymous();
        p.scopes = vec!["read".into(), "write".into()];
        assert!(p.has_scope("read"));
        assert!(p.has_scope("write"));
        assert!(!p.has_scope("admin"));
        assert!(!p.has_scope("re")); // no substring match
    }

    #[test]
    fn tenant_id_shim_returns_str() {
        let p = Principal::stdio_local();
        assert_eq!(p.tenant_id(), "local");
    }
}
