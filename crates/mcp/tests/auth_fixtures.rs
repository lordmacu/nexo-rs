//! Phase 76.3 — JWT + JWKS fixtures shared by `http_auth_test.rs`.
//!
//! Each test boots a self-contained RSA keypair and an in-process
//! axum JWKS server so we can drive the bearer-JWT authenticator
//! end-to-end without any external IdP.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{routing::get, Json, Router};
use base64::Engine;
use jsonwebtoken::{EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::pkcs8::EncodePublicKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

pub struct RsaFixture {
    pub kid: String,
    pub priv_der: Vec<u8>,
    pub pub_jwk: Jwk,
}

#[derive(Clone, Serialize)]
pub struct Jwk {
    pub kty: String,
    pub kid: String,
    pub alg: String,
    #[serde(rename = "use")]
    pub use_: String,
    pub n: String,
    pub e: String,
}

#[derive(Serialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

impl RsaFixture {
    pub fn generate(kid: &str) -> Self {
        let mut rng = rand::rngs::OsRng;
        let priv_key = RsaPrivateKey::new(&mut rng, 2048).expect("rsa keygen");
        let pub_key = RsaPublicKey::from(&priv_key);
        let der = priv_key
            .to_pkcs1_der()
            .expect("pkcs1 der")
            .as_bytes()
            .to_vec();
        let _ = pub_key.to_public_key_der().expect("spki"); // sanity
        let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pub_key.n().to_bytes_be());
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pub_key.e().to_bytes_be());
        Self {
            kid: kid.into(),
            priv_der: der,
            pub_jwk: Jwk {
                kty: "RSA".into(),
                kid: kid.into(),
                alg: "RS256".into(),
                use_: "sig".into(),
                n,
                e,
            },
        }
    }

    pub fn sign_rs256(&self, claims: &serde_json::Value) -> String {
        let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        let key = EncodingKey::from_rsa_der(&self.priv_der);
        jsonwebtoken::encode(&header, claims, &key).expect("sign rs256")
    }

    pub fn sign_with_alg(
        &self,
        claims: &serde_json::Value,
        alg: jsonwebtoken::Algorithm,
        key_override: Option<&[u8]>,
    ) -> String {
        let mut header = Header::new(alg);
        header.kid = Some(self.kid.clone());
        let key = match (alg, key_override) {
            (jsonwebtoken::Algorithm::HS256, Some(k)) => EncodingKey::from_secret(k),
            _ => EncodingKey::from_rsa_der(&self.priv_der),
        };
        jsonwebtoken::encode(&header, claims, &key).expect("sign")
    }
}

pub struct JwksServer {
    pub addr: SocketAddr,
    pub shutdown: CancellationToken,
    pub join: tokio::task::JoinHandle<()>,
}

pub async fn spawn_jwks(jwks: JwkSet) -> JwksServer {
    let shared = Arc::new(jwks);
    let app = Router::new().route(
        "/jwks.json",
        get({
            let shared = shared.clone();
            move || {
                let shared = shared.clone();
                async move { Json(serde_json::json!({ "keys": shared.keys })) }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = CancellationToken::new();
    let token_inner = token.clone();
    let join = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { token_inner.cancelled().await })
            .await
            .unwrap();
    });
    JwksServer {
        addr,
        shutdown: token,
        join,
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn standard_claims(
    iss: &str,
    aud: &str,
    sub: &str,
    tenant: &str,
    scopes: &[&str],
    exp: u64,
) -> serde_json::Value {
    serde_json::json!({
        "iss": iss,
        "aud": aud,
        "sub": sub,
        "tenant_id": tenant,
        "scope": scopes.join(" "),
        "exp": exp,
        "iat": now(),
        "nbf": now(),
    })
}
