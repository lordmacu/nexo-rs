#![allow(clippy::all)] // Phase 76 scaffolding — re-enable when 76.x fully shipped

//! Phase 76.3 — adversarial HTTP-level auth tests.
//!
//! Booted server uses 127.0.0.1:0 so each test gets a kernel-assigned
//! port. The test handler is intentionally trivial — what we care
//! about is the auth gate, not the dispatch path.

#![allow(dead_code)]

mod auth_fixtures;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use auth_fixtures::{spawn_jwks, standard_claims, JwkSet, JwksServer, RsaFixture};
use nexo_mcp::server::auth::bearer_jwt::JwtConfig;
use nexo_mcp::server::auth::mutual_tls::MutualTlsConfig;
use nexo_mcp::server::auth::AuthConfig;
use nexo_mcp::server::http_config::HttpTransportConfig;
use nexo_mcp::types::{McpContent, McpServerInfo, McpTool, McpToolResult};
use nexo_mcp::{start_http_server, HttpServerHandle, McpError, McpServerHandler};
use reqwest::Client;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct EchoHandler;

#[async_trait]
impl McpServerHandler for EchoHandler {
    fn server_info(&self) -> McpServerInfo {
        McpServerInfo {
            name: "auth-test".into(),
            version: "0.0.1".into(),
        }
    }
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![])
    }
    async fn call_tool(&self, _: &str, _: Value) -> Result<McpToolResult, McpError> {
        Ok(McpToolResult {
            content: vec![McpContent::Text { text: "ok".into() }],
            is_error: false,
            structured_content: None,
        })
    }
}

fn cfg_with_auth(auth: AuthConfig) -> HttpTransportConfig {
    let mut c = HttpTransportConfig::default();
    c.enabled = true;
    c.bind = "127.0.0.1:0".parse().unwrap();
    c.auth = Some(auth);
    c
}

async fn boot(cfg: HttpTransportConfig) -> (HttpServerHandle, Client, CancellationToken) {
    let shutdown = CancellationToken::new();
    let handle = start_http_server(EchoHandler, cfg, shutdown.clone())
        .await
        .expect("server boot");
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    (handle, client, shutdown)
}

async fn teardown(handle: HttpServerHandle, token: CancellationToken) {
    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), handle.join).await;
}

async fn post_initialize(
    client: &Client,
    addr: SocketAddr,
    headers: Vec<(&str, String)>,
) -> reqwest::Response {
    let url = format!("http://{}/mcp", addr);
    let mut req = client.post(&url).json(&serde_json::json!({
        "jsonrpc":"2.0","method":"initialize","params":{},"id":1
    }));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    req.send().await.unwrap()
}

// --- StaticToken --------------------------------------------------

#[tokio::test]
async fn static_token_missing_header_returns_401() {
    let cfg = cfg_with_auth(AuthConfig::StaticToken {
        token: Some("s3cret".into()),
        token_env: None,
        tenant: None,
    });
    let (handle, client, shut) = boot(cfg).await;
    let resp = post_initialize(&client, handle.bind_addr, vec![]).await;
    assert_eq!(resp.status().as_u16(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32001);
    assert_eq!(body["error"]["message"], "unauthorized");
    teardown(handle, shut).await;
}

#[tokio::test]
async fn static_token_wrong_value_returns_uniform_401() {
    let cfg = cfg_with_auth(AuthConfig::StaticToken {
        token: Some("s3cret-12345".into()),
        token_env: None,
        tenant: None,
    });
    let (handle, client, shut) = boot(cfg).await;
    // wrong same-length value (consteq path)
    let r1 = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", "Bearer wrong-tok-12345".into())],
    )
    .await;
    // wrong different-length value (length-shortcut)
    let r2 = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", "Bearer x".into())],
    )
    .await;
    assert_eq!(r1.status().as_u16(), 401);
    assert_eq!(r2.status().as_u16(), 401);
    let b1: Value = r1.json().await.unwrap();
    let b2: Value = r2.json().await.unwrap();
    assert_eq!(b1, b2, "all 401s must be byte-identical (anti-enumeration)");
    teardown(handle, shut).await;
}

#[tokio::test]
async fn static_token_correct_passes() {
    let cfg = cfg_with_auth(AuthConfig::StaticToken {
        token: Some("s3cret".into()),
        token_env: None,
        tenant: None,
    });
    let (handle, client, shut) = boot(cfg).await;
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", "Bearer s3cret".into())],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    teardown(handle, shut).await;
}

#[tokio::test]
async fn static_token_alt_header_passes() {
    let cfg = cfg_with_auth(AuthConfig::StaticToken {
        token: Some("s3cret".into()),
        token_env: None,
        tenant: None,
    });
    let (handle, client, shut) = boot(cfg).await;
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("mcp-auth-token", "s3cret".into())],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    teardown(handle, shut).await;
}

// --- mTLS from header ---------------------------------------------

#[tokio::test]
async fn mtls_from_header_missing_cn_returns_401() {
    let cfg = cfg_with_auth(AuthConfig::MutualTls(MutualTlsConfig::FromHeader {
        header_name: "x-client-cert-cn".into(),
        cn_allowlist: vec!["agent-1.internal".into()],
        cn_to_tenant: None,
    }));
    let (handle, client, shut) = boot(cfg).await;
    let resp = post_initialize(&client, handle.bind_addr, vec![]).await;
    assert_eq!(resp.status().as_u16(), 401);
    teardown(handle, shut).await;
}

#[tokio::test]
async fn mtls_from_header_wrong_cn_returns_401() {
    let cfg = cfg_with_auth(AuthConfig::MutualTls(MutualTlsConfig::FromHeader {
        header_name: "x-client-cert-cn".into(),
        cn_allowlist: vec!["agent-1.internal".into()],
        cn_to_tenant: None,
    }));
    let (handle, client, shut) = boot(cfg).await;
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("x-client-cert-cn", "evil.example.com".into())],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    teardown(handle, shut).await;
}

#[tokio::test]
async fn mtls_from_header_allowlisted_cn_passes() {
    // CNs with dots require a `cn_to_tenant` remap (Phase 76.4) —
    // a dotted CN cannot pass the strict `TenantId` validator on
    // its own.
    let cfg = cfg_with_auth(AuthConfig::MutualTls(MutualTlsConfig::FromHeader {
        header_name: "x-client-cert-cn".into(),
        cn_allowlist: vec!["agent-1.internal".into(), "agent-2.internal".into()],
        cn_to_tenant: Some(
            [
                ("agent-1.internal".to_string(), "tenant-a".to_string()),
                ("agent-2.internal".to_string(), "tenant-b".to_string()),
            ]
            .into_iter()
            .collect(),
        ),
    }));
    let (handle, client, shut) = boot(cfg).await;
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("x-client-cert-cn", "agent-2.internal".into())],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    teardown(handle, shut).await;
}

// --- Bearer JWT ---------------------------------------------------

async fn boot_with_jwt(
    fixture: Arc<RsaFixture>,
    audiences: Vec<String>,
    algorithms: Vec<String>,
) -> (JwksServer, HttpServerHandle, Client, CancellationToken) {
    let jwks = JwkSet {
        keys: vec![fixture.pub_jwk.clone()],
    };
    let jwks_server = spawn_jwks(jwks).await;
    let cfg = cfg_with_auth(AuthConfig::BearerJwt(JwtConfig {
        jwks_url: format!("http://{}/jwks.json", jwks_server.addr),
        jwks_cache_ttl_secs: 300,
        jwks_refresh_cooldown_secs: 1,
        algorithms,
        issuer: "https://idp.test/".into(),
        audiences,
        tenant_claim: "tenant_id".into(),
        scopes_claim: "scope".into(),
        leeway_secs: 5,
    }));
    let (handle, client, shut) = boot(cfg).await;
    (jwks_server, handle, client, shut)
}

#[tokio::test]
async fn jwt_valid_token_passes() {
    let fx = Arc::new(RsaFixture::generate("k1"));
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx.clone(), vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let now = auth_fixtures::now();
    let claims = standard_claims(
        "https://idp.test/",
        "nexo-mcp",
        "subj-1",
        "tenant-a",
        &["mcp.read"],
        now + 600,
    );
    let token = fx.sign_rs256(&claims);
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_missing_token_returns_401() {
    let fx = Arc::new(RsaFixture::generate("k1"));
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx, vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let resp = post_initialize(&client, handle.bind_addr, vec![]).await;
    assert_eq!(resp.status().as_u16(), 401);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_malformed_returns_401() {
    let fx = Arc::new(RsaFixture::generate("k1"));
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx, vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", "Bearer not.a.jwt".into())],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_signed_by_unknown_key_returns_401() {
    let fx_good = Arc::new(RsaFixture::generate("k1"));
    let fx_evil = RsaFixture::generate("k1"); // same kid, different key
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx_good, vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let now = auth_fixtures::now();
    let claims = standard_claims(
        "https://idp.test/",
        "nexo-mcp",
        "evil",
        "tenant-a",
        &["mcp.read"],
        now + 600,
    );
    let token = fx_evil.sign_rs256(&claims);
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_wrong_audience_returns_401() {
    let fx = Arc::new(RsaFixture::generate("k1"));
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx.clone(), vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let now = auth_fixtures::now();
    let claims = standard_claims(
        "https://idp.test/",
        "wrong-aud",
        "subj-1",
        "tenant-a",
        &["mcp.read"],
        now + 600,
    );
    let token = fx.sign_rs256(&claims);
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_expired_returns_401() {
    let fx = Arc::new(RsaFixture::generate("k1"));
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx.clone(), vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let now = auth_fixtures::now();
    let claims = standard_claims(
        "https://idp.test/",
        "nexo-mcp",
        "subj-1",
        "tenant-a",
        &["mcp.read"],
        now - 3600, // expired 1h ago
    );
    let token = fx.sign_rs256(&claims);
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_wrong_issuer_returns_401() {
    let fx = Arc::new(RsaFixture::generate("k1"));
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx.clone(), vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let now = auth_fixtures::now();
    let claims = standard_claims(
        "https://attacker.example/",
        "nexo-mcp",
        "subj-1",
        "tenant-a",
        &["mcp.read"],
        now + 600,
    );
    let token = fx.sign_rs256(&claims);
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_alg_confusion_hs_token_rejected_when_only_rs_allowed() {
    // Server allows only RS256. Attacker forges an HS256 token using
    // the public-key bytes from JWKS as the HMAC secret. The
    // jsonwebtoken decoder MUST refuse the HS algorithm because the
    // configured allowlist is RS-only.
    let fx = Arc::new(RsaFixture::generate("k1"));
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx.clone(), vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let now = auth_fixtures::now();
    let claims = standard_claims(
        "https://idp.test/",
        "nexo-mcp",
        "subj-1",
        "tenant-a",
        &["mcp.read"],
        now + 600,
    );
    // Fake HS256 signing using a small static secret. The runtime
    // rejects on alg-not-in-allowlist before even trying to verify.
    let token = fx.sign_with_alg(
        &claims,
        jsonwebtoken::Algorithm::HS256,
        Some(b"public-bytes-as-secret"),
    );
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_jwks_unreachable_returns_503() {
    // Bind a JWKS server, then immediately tear it down so the
    // first authenticated request can't reach it.
    let fx = RsaFixture::generate("k1");
    let jwks_server = spawn_jwks(JwkSet {
        keys: vec![fx.pub_jwk.clone()],
    })
    .await;
    let url = format!("http://{}/jwks.json", jwks_server.addr);
    jwks_server.shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), jwks_server.join).await;

    let cfg = cfg_with_auth(AuthConfig::BearerJwt(JwtConfig {
        jwks_url: url,
        jwks_cache_ttl_secs: 300,
        jwks_refresh_cooldown_secs: 1,
        algorithms: vec!["RS256".into()],
        issuer: "https://idp.test/".into(),
        audiences: vec!["nexo-mcp".into()],
        tenant_claim: "tenant_id".into(),
        scopes_claim: "scope".into(),
        leeway_secs: 5,
    }));
    let (handle, client, shut) = boot(cfg).await;
    let now = auth_fixtures::now();
    let claims = standard_claims(
        "https://idp.test/",
        "nexo-mcp",
        "subj-1",
        "tenant-a",
        &["mcp.read"],
        now + 600,
    );
    let token = fx.sign_rs256(&claims);
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 503);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32099);
    teardown(handle, shut).await;
}

// --- Boot validation ----------------------------------------------

#[tokio::test]
async fn alg_none_in_yaml_refused_at_boot() {
    let cfg = cfg_with_auth(AuthConfig::BearerJwt(JwtConfig {
        jwks_url: "http://127.0.0.1:1/jwks.json".into(),
        jwks_cache_ttl_secs: 300,
        jwks_refresh_cooldown_secs: 1,
        algorithms: vec!["none".into()],
        issuer: "https://idp.test/".into(),
        audiences: vec!["nexo-mcp".into()],
        tenant_claim: "tenant_id".into(),
        scopes_claim: "scope".into(),
        leeway_secs: 5,
    }));
    let shutdown = CancellationToken::new();
    let res = start_http_server(EchoHandler, cfg, shutdown).await;
    assert!(res.is_err(), "alg=none must be refused at boot");
}

#[tokio::test]
async fn empty_algorithms_refused_at_boot() {
    let cfg = cfg_with_auth(AuthConfig::BearerJwt(JwtConfig {
        jwks_url: "http://127.0.0.1:1/jwks.json".into(),
        jwks_cache_ttl_secs: 300,
        jwks_refresh_cooldown_secs: 1,
        algorithms: vec![],
        issuer: "https://idp.test/".into(),
        audiences: vec!["nexo-mcp".into()],
        tenant_claim: "tenant_id".into(),
        scopes_claim: "scope".into(),
        leeway_secs: 5,
    }));
    let shutdown = CancellationToken::new();
    let res = start_http_server(EchoHandler, cfg, shutdown).await;
    assert!(res.is_err(), "empty algorithms list must be refused");
}

#[tokio::test]
async fn mtls_non_loopback_refused_at_boot() {
    let mut cfg = HttpTransportConfig::default();
    cfg.enabled = true;
    cfg.bind = "0.0.0.0:0".parse().unwrap();
    cfg.auth = Some(AuthConfig::MutualTls(MutualTlsConfig::FromHeader {
        header_name: "x-client-cert-cn".into(),
        cn_allowlist: vec!["agent-1.internal".into()],
        cn_to_tenant: None,
    }));
    let shutdown = CancellationToken::new();
    let res = start_http_server(EchoHandler, cfg, shutdown).await;
    assert!(res.is_err(), "mtls.from_header on 0.0.0.0 must be refused");
}

#[tokio::test]
async fn auth_none_non_loopback_refused_at_boot() {
    let mut cfg = HttpTransportConfig::default();
    cfg.enabled = true;
    cfg.bind = "0.0.0.0:0".parse().unwrap();
    cfg.auth = Some(AuthConfig::None);
    let shutdown = CancellationToken::new();
    let res = start_http_server(EchoHandler, cfg, shutdown).await;
    assert!(res.is_err(), "auth=none on 0.0.0.0 must be refused at boot");
}

// --- Phase 76.4 tenant flow ---------------------------------------

#[tokio::test]
async fn static_token_with_invalid_tenant_refused_at_boot() {
    let cfg = cfg_with_auth(AuthConfig::StaticToken {
        token: Some("s3cret".into()),
        token_env: None,
        tenant: Some("Tenant.Bad".into()), // uppercase + dot
    });
    let shutdown = CancellationToken::new();
    let res = start_http_server(EchoHandler, cfg, shutdown).await;
    assert!(res.is_err(), "invalid tenant id must be refused at boot");
}

#[tokio::test]
async fn static_token_with_valid_tenant_smoke() {
    // Boots cleanly — full tenant flow is exercised by
    // multitenant_isolation_test.rs.
    let cfg = cfg_with_auth(AuthConfig::StaticToken {
        token: Some("s3cret".into()),
        token_env: None,
        tenant: Some("prod-corp".into()),
    });
    let (handle, client, shut) = boot(cfg).await;
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", "Bearer s3cret".into())],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    teardown(handle, shut).await;
}

#[tokio::test]
async fn mtls_cn_with_dot_without_remap_returns_401() {
    let cfg = cfg_with_auth(AuthConfig::MutualTls(MutualTlsConfig::FromHeader {
        header_name: "x-client-cert-cn".into(),
        cn_allowlist: vec!["agent-1.internal".into()],
        cn_to_tenant: None,
    }));
    let (handle, client, shut) = boot(cfg).await;
    // CN passes the allowlist but fails TenantId::parse (dot).
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("x-client-cert-cn", "agent-1.internal".into())],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    teardown(handle, shut).await;
}

#[tokio::test]
async fn mtls_cn_without_dot_passes_when_valid_tenant() {
    // CN that IS a valid tenant id — no remap needed.
    let cfg = cfg_with_auth(AuthConfig::MutualTls(MutualTlsConfig::FromHeader {
        header_name: "x-client-cert-cn".into(),
        cn_allowlist: vec!["agent-x".into()],
        cn_to_tenant: None,
    }));
    let (handle, client, shut) = boot(cfg).await;
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("x-client-cert-cn", "agent-x".into())],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    teardown(handle, shut).await;
}

#[tokio::test]
async fn mtls_cn_to_tenant_remap_passes_dotted_cn() {
    let cfg = cfg_with_auth(AuthConfig::MutualTls(MutualTlsConfig::FromHeader {
        header_name: "x-client-cert-cn".into(),
        cn_allowlist: vec!["agent-1.internal".into()],
        cn_to_tenant: Some(
            [("agent-1.internal".to_string(), "tenant-a".to_string())]
                .into_iter()
                .collect(),
        ),
    }));
    let (handle, client, shut) = boot(cfg).await;
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("x-client-cert-cn", "agent-1.internal".into())],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_invalid_tenant_claim_format_returns_401() {
    let fx = std::sync::Arc::new(RsaFixture::generate("k1"));
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx.clone(), vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let now = auth_fixtures::now();
    // tenant_id = "BadCase" — uppercase, fails TenantId::parse.
    let claims = standard_claims(
        "https://idp.test/",
        "nexo-mcp",
        "subj-1",
        "BadCase",
        &["mcp.read"],
        now + 600,
    );
    let token = fx.sign_rs256(&claims);
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn jwt_missing_tenant_claim_returns_401() {
    let fx = std::sync::Arc::new(RsaFixture::generate("k1"));
    let (jwks, handle, client, shut) =
        boot_with_jwt(fx.clone(), vec!["nexo-mcp".into()], vec!["RS256".into()]).await;
    let now = auth_fixtures::now();
    // No tenant_id claim at all.
    let claims = serde_json::json!({
        "iss": "https://idp.test/",
        "aud": "nexo-mcp",
        "sub": "subj-1",
        "scope": "mcp.read",
        "exp": now + 600,
        "iat": now,
        "nbf": now,
    });
    let token = fx.sign_rs256(&claims);
    let resp = post_initialize(
        &client,
        handle.bind_addr,
        vec![("authorization", format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);
    jwks.shutdown.cancel();
    teardown(handle, shut).await;
}

#[tokio::test]
async fn mtls_cn_to_tenant_with_invalid_tenant_value_refused_at_boot() {
    let cfg = cfg_with_auth(AuthConfig::MutualTls(MutualTlsConfig::FromHeader {
        header_name: "x-client-cert-cn".into(),
        cn_allowlist: vec!["agent-1".into()],
        cn_to_tenant: Some(
            [("agent-1".to_string(), "Bad.Tenant".to_string())]
                .into_iter()
                .collect(),
        ),
    }));
    let shutdown = CancellationToken::new();
    let res = start_http_server(EchoHandler, cfg, shutdown).await;
    assert!(
        res.is_err(),
        "invalid cn_to_tenant value must be refused at boot"
    );
}
