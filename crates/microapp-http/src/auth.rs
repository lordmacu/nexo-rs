//! Bearer auth middleware + runtime-mutable token state.
//!
//! Three pieces tightly bound to the daemon's
//! `nexo/notify/token_rotated` notification:
//!
//! 1. [`token_hash`] — sha256 hex truncated to 16 chars.
//!    Byte-parity with `nexo_setup::http_supervisor::token_hash`,
//!    so the daemon-side hash + microapp-side hash always agree.
//! 2. [`LiveTokenState`] — `ArcSwap<String>` for both the bearer
//!    token and the operator hash. Lock-free reads from every
//!    request; rotations are serialised by an internal
//!    `tokio::Mutex<()>` to keep the compare-and-swap atomic.
//! 3. [`require_bearer`] — `axum::middleware` that pulls the
//!    current token from `LiveTokenState` and rejects requests
//!    that don't present it (constant-time compare via `subtle`).
//!    Falls back to a `?token=<value>` query parameter for SSE
//!    clients (EventSource can't set headers).
//! 4. [`handle_token_rotated`] — listener body for
//!    `nexo/notify/token_rotated` that delegates to
//!    `LiveTokenState::rotate`.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use nexo_tool_meta::http_server::TokenRotated;
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

const BEARER_PREFIX: &str = "Bearer ";

/// SHA-256 hex of `token`, truncated to 16 chars (64 bits — enough
/// for distinguishing operator identities in the audit log without
/// exposing the bearer itself). Byte-parity with
/// `nexo_setup::http_supervisor::token_hash`.
pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex_encode_lower(&digest[..8])
}

/// Lower-hex encode `bytes`. Pulled inline so the crate doesn't
/// drag in the `hex` crate for one helper.
fn hex_encode_lower(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0x0f) as usize] as char);
    }
    out
}

/// Runtime-mutable bearer token + operator-hash state.
///
/// Reads are lock-free per-request via `ArcSwap::load_full()`;
/// the rotate path serialises with an internal `Mutex<()>` so
/// the compare-and-swap against `old_hash` stays atomic.
pub struct LiveTokenState {
    /// Current bearer token. Compared against incoming
    /// `Authorization: Bearer …` headers via `subtle::ConstantTimeEq`.
    pub token: ArcSwap<String>,
    /// Current operator-token-hash (sha256 hex truncated to 16
    /// chars). Microapps register this as the source for
    /// `AdminClient::set_operator_token_hash` so SDK calls stamp
    /// the rotated identity automatically (Phase 82.10.m).
    pub operator_token_hash: ArcSwap<String>,
    rotate_lock: Mutex<()>,
}

impl LiveTokenState {
    /// Build from the boot config snapshot (token + pre-computed
    /// hash). Caller is responsible for invoking [`token_hash`]
    /// on the bearer to produce the second argument; we don't
    /// recompute internally because some callers need to stash
    /// the hash on the daemon-side `HttpServerConfig` before
    /// the microapp boots.
    pub fn from_strings(token: &str, operator_token_hash: &str) -> Arc<Self> {
        Arc::new(Self {
            token: ArcSwap::from_pointee(token.to_string()),
            operator_token_hash: ArcSwap::from_pointee(operator_token_hash.to_string()),
            rotate_lock: Mutex::new(()),
        })
    }

    /// Atomic swap. Validates `old_hash` matches the current
    /// hash + non-empty `new`; on success replaces both fields
    /// (token + freshly-computed hash via [`token_hash`]).
    ///
    /// Returns `true` when the swap actually fired. False on
    /// either rejection (caller already has all the context it
    /// needs from the warn log).
    pub async fn rotate(&self, old_hash: &str, new: &str) -> bool {
        if new.is_empty() {
            tracing::warn!("token_rotated: empty `new` token — ignoring");
            return false;
        }
        let _g = self.rotate_lock.lock().await;
        let current = self.operator_token_hash.load_full();
        if old_hash != current.as_str() {
            tracing::warn!(
                expected = %current,
                received = %old_hash,
                "token_rotated: hash mismatch — ignoring"
            );
            return false;
        }
        let new_hash = token_hash(new);
        self.token.store(Arc::new(new.to_string()));
        self.operator_token_hash.store(Arc::new(new_hash));
        tracing::info!("bearer token rotated successfully");
        true
    }
}

/// Listener entry point for `nexo/notify/token_rotated` frames.
/// Deserialises the wire payload (from `nexo_tool_meta`), then
/// delegates to [`LiveTokenState::rotate`]. Malformed payloads
/// warn + return without panicking — the SDK's notification
/// listener registry runs this in a `tokio::spawn` task isolated
/// from the dispatch loop.
pub async fn handle_token_rotated(state: Arc<LiveTokenState>, params: Value) {
    let evt: TokenRotated = match serde_json::from_value(params) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "token_rotated: malformed payload");
            return;
        }
    };
    state.rotate(&evt.old_hash, &evt.new).await;
}

/// Reject any request whose `Authorization` header doesn't carry
/// the expected bearer token. Used as
/// `axum::middleware::from_fn_with_state(state, require_bearer)`.
///
/// Reads from the runtime-mutable `LiveTokenState` per request:
/// `load_full()` returns an `Arc<String>` snapshot without any
/// lock or yield point, so a `nexo/notify/token_rotated` swap
/// is visible on the very next request.
pub async fn require_bearer(
    State(state): State<Arc<LiveTokenState>>,
    req: Request,
    next: Next,
) -> Response {
    let token = state.token.load_full();
    if !is_authorized(&req, token.as_str()) {
        return unauthorized();
    }
    next.run(req).await
}

fn is_authorized(req: &Request, expected: &str) -> bool {
    // Primary path: `Authorization: Bearer <token>` header.
    if let Some(header) = req.headers().get("authorization") {
        if let Ok(s) = header.to_str() {
            if let Some(presented) = s.strip_prefix(BEARER_PREFIX) {
                if presented.as_bytes().ct_eq(expected.as_bytes()).into() {
                    return true;
                }
            }
        }
    }
    // Fallback: `?token=<value>` query parameter. EventSource
    // (the browser SSE primitive) cannot set custom headers, so
    // SPA clients pass the bearer as a query param for SSE
    // routes. Same constant-time compare; same accept/reject
    // outcome.
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("token=") {
                let decoded = url_decode(value);
                if decoded.as_bytes().ct_eq(expected.as_bytes()).into() {
                    return true;
                }
            }
        }
    }
    false
}

/// Minimal URL-decoder for the bearer query fallback. Handles
/// `%XX` hex escapes; leaves non-encoded bytes alone. Avoids
/// pulling a `urlencoding` crate for this single use.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unauthorized() -> Response {
    let mut res = (StatusCode::UNAUTHORIZED, "Unauthorized\n").into_response();
    res.headers_mut().insert(
        "www-authenticate",
        HeaderValue::from_static(r#"Bearer realm="nexo-microapp""#),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as AxumRequest, StatusCode};
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use axum::Router;
    use serde_json::json;
    use tower::ServiceExt;

    // ── token_hash ───────────────────────────────────────────────

    #[test]
    fn token_hash_is_16_hex_chars() {
        let h = token_hash("super-secret");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn token_hash_is_deterministic() {
        assert_eq!(token_hash("abc"), token_hash("abc"));
        assert_ne!(token_hash("abc"), token_hash("abd"));
    }

    // ── LiveTokenState ───────────────────────────────────────────

    fn mk(token: &str) -> Arc<LiveTokenState> {
        let hash = token_hash(token);
        LiveTokenState::from_strings(token, &hash)
    }

    #[tokio::test]
    async fn rotate_swaps_on_valid_notification() {
        let state = mk("old-token");
        let old_hash = token_hash("old-token");
        let did = state.rotate(&old_hash, "new-token").await;
        assert!(did, "valid rotation should fire");
        assert_eq!(state.token.load_full().as_str(), "new-token");
        assert_eq!(
            state.operator_token_hash.load_full().as_str(),
            token_hash("new-token")
        );
    }

    #[tokio::test]
    async fn rotate_ignores_stale_notification() {
        let state = mk("old-token");
        let did = state.rotate("not-the-current-hash", "new-token").await;
        assert!(!did, "stale notification should not swap");
        assert_eq!(state.token.load_full().as_str(), "old-token");
    }

    #[tokio::test]
    async fn rotate_ignores_empty_new() {
        let state = mk("old-token");
        let old_hash = token_hash("old-token");
        let did = state.rotate(&old_hash, "").await;
        assert!(!did, "empty new token should not swap");
        assert_eq!(state.token.load_full().as_str(), "old-token");
    }

    #[tokio::test]
    async fn handle_token_rotated_ignores_malformed_payload() {
        let state = mk("old-token");
        handle_token_rotated(Arc::clone(&state), Value::Null).await;
        assert_eq!(state.token.load_full().as_str(), "old-token");
    }

    #[tokio::test]
    async fn handle_token_rotated_swaps_on_valid_payload() {
        let state = mk("old-token");
        let old_hash = token_hash("old-token");
        let payload = json!({
            "old_hash": old_hash,
            "new": "fresh-token",
        });
        handle_token_rotated(Arc::clone(&state), payload).await;
        assert_eq!(state.token.load_full().as_str(), "fresh-token");
        assert_eq!(
            state.operator_token_hash.load_full().as_str(),
            token_hash("fresh-token")
        );
    }

    // ── require_bearer middleware ────────────────────────────────

    fn protected_router(token: &str) -> Router {
        let state = LiveTokenState::from_strings(token, &token_hash(token));
        Router::new()
            .route("/secret", get(|| async { "ok" }))
            .layer(from_fn_with_state(state, require_bearer))
    }

    #[tokio::test]
    async fn missing_header_returns_401_with_challenge() {
        let app = protected_router("xyz");
        let res = app
            .oneshot(
                AxumRequest::builder()
                    .uri("/secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let www_auth = res.headers().get("www-authenticate").unwrap();
        assert!(www_auth.to_str().unwrap().starts_with("Bearer "));
    }

    #[tokio::test]
    async fn correct_bearer_passes_through() {
        let app = protected_router("xyz");
        let res = app
            .oneshot(
                AxumRequest::builder()
                    .uri("/secret")
                    .header("authorization", "Bearer xyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn wrong_bearer_returns_401() {
        let app = protected_router("right");
        let res = app
            .oneshot(
                AxumRequest::builder()
                    .uri("/secret")
                    .header("authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn query_token_param_passes_when_correct() {
        let app = protected_router("xyz");
        let res = app
            .oneshot(
                AxumRequest::builder()
                    .uri("/secret?token=xyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn query_token_param_url_decoded() {
        let app = protected_router("a/b+c");
        let res = app
            .oneshot(
                AxumRequest::builder()
                    .uri("/secret?token=a%2Fb%2Bc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_middleware_rejects_after_rotation() {
        let state = LiveTokenState::from_strings("old-token", &token_hash("old-token"));
        let app = Router::new()
            .route("/secret", get(|| async { "ok" }))
            .layer(from_fn_with_state(Arc::clone(&state), require_bearer));

        // Pre-rotation: old token works.
        let res = app
            .clone()
            .oneshot(
                AxumRequest::builder()
                    .uri("/secret")
                    .header("authorization", "Bearer old-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Rotate.
        let did = state
            .rotate(&token_hash("old-token"), "new-token")
            .await;
        assert!(did);

        // Post-rotation: old token rejected.
        let res = app
            .oneshot(
                AxumRequest::builder()
                    .uri("/secret")
                    .header("authorization", "Bearer old-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
