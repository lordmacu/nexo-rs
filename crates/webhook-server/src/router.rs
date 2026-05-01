//! Axum router + per-source 5-gate handler.
//!
//! `build_router(cfg, dispatcher)` returns a fully wired
//! `axum::Router` plus a `RouterState` snapshot the caller can
//! later swap (Step 7 hot-reload). Each route is built per-source
//! with its own concurrency semaphore and rate-limit bucket map.
//!
//! Pipeline (in order):
//!   1. axum router only matches POST `<path>` — every other
//!      method falls through to a default 405 handler.
//!   2. `RequestBodyLimitLayer::max(per_source_cap)` rejects
//!      oversize bodies with 413 before HMAC compute.
//!   3. `Arc<Semaphore>::try_acquire_owned()` — `503 Retry-After: 1`
//!      when in-flight cap reached.
//!   4. `(source_id, client_ip)` token-bucket — `429` when empty.
//!   5. `WebhookHandler::handle` (signature/event-kind/payload).
//!      Maps `RejectReason` → typed HTTP code.
//!   6. Dispatch to `WebhookDispatcher` impl. Broker failure → 502;
//!      envelope-rejected → 422.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes as AxumBytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use nexo_config::types::webhook_receiver::{WebhookConfigError, WebhookServerConfig};
use nexo_webhook_receiver::{
    resolve_request_client_ip, ProxyHeaders, RejectReason, WebhookDispatcher, WebhookHandler,
};
use thiserror::Error;
use tokio::sync::Semaphore;
use tower_http::limit::RequestBodyLimitLayer;

use crate::rate_limit::{ClientBucketKey, ClientBucketMap};

/// Reasons [`build_router`] can fail.
///
/// `#[non_exhaustive]` — operator-facing diagnostic, future
/// build-time errors land as semver-minor.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum WebhookRouterError {
    /// Invalid config (duplicate ids, reserved bind port, etc.).
    #[error("config invalid: {0}")]
    Config(#[from] WebhookConfigError),
    /// Caller passed an enabled config with zero declared sources.
    #[error("no sources configured (router would expose no routes)")]
    NoSources,
}

/// Per-source runtime state.
pub struct SourceState {
    /// Webhook source identifier (`WebhookSourceConfig.id`).
    pub source_id: String,
    /// HTTP path the route is mounted at.
    pub path: String,
    /// Phase 80.12 verifier shared with the request handler.
    pub handler: Arc<WebhookHandler>,
    /// `None` when `concurrency_cap == 0` — semaphore acquisition
    /// skipped (unbounded).
    pub semaphore: Option<Arc<Semaphore>>,
    /// `None` when neither global nor per-source rate limit is
    /// active.
    pub rate_limit: Option<Arc<ClientBucketMap>>,
    /// Pre-rendered NATS subject template (with `${event_kind}`).
    pub publish_to: String,
}

/// Captured router state — used by [`crate::reevaluate`] to
/// compute the kept/added/evicted delta on hot-reload.
pub struct RouterState {
    /// Source id → mounted state, for cheap lookup.
    pub sources: HashMap<String, Arc<SourceState>>,
    /// CIDRs trusted to forward `X-Forwarded-For`.
    pub trusted_proxies: Vec<ipnetwork::IpNetwork>,
    /// Whether to honour `X-Real-IP` when XFF chain is empty.
    pub allow_realip_fallback: bool,
}

impl RouterState {
    /// Sorted list of mounted source ids.
    pub fn source_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.sources.keys().cloned().collect();
        ids.sort();
        ids
    }
}

/// Build the axum router from a validated config + a dispatcher.
/// Caller is expected to have called `cfg.validate()` already; the
/// fn re-validates as a defensive belt-and-suspenders.
pub fn build_router(
    cfg: &WebhookServerConfig,
    dispatcher: Arc<dyn WebhookDispatcher>,
) -> Result<(Router, Arc<RouterState>), WebhookRouterError> {
    cfg.validate()?;
    if cfg.sources.is_empty() {
        return Err(WebhookRouterError::NoSources);
    }

    let mut sources: HashMap<String, Arc<SourceState>> = HashMap::new();
    let mut router: Router<()> = Router::new();

    for s in &cfg.sources {
        let body_cap = cfg.resolve_body_cap(&s.source.id);
        let concurrency_cap = cfg.resolve_concurrency_cap(&s.source.id);
        let rl = cfg.resolve_rate_limit(&s.source.id);

        let semaphore = if concurrency_cap == 0 {
            None
        } else {
            Some(Arc::new(Semaphore::new(concurrency_cap as usize)))
        };

        let rate_limit = match rl {
            Some(rl_cfg) if rl_cfg.is_active() => {
                Some(Arc::new(ClientBucketMap::new(rl_cfg)))
            }
            _ => None,
        };

        let handler = Arc::new(WebhookHandler::new(s.source.clone()));

        let state = Arc::new(SourceState {
            source_id: s.source.id.clone(),
            path: s.source.path.clone(),
            handler,
            semaphore,
            rate_limit,
            publish_to: s.source.publish_to.clone(),
        });

        sources.insert(s.source.id.clone(), Arc::clone(&state));

        let route_state = HandlerCtx {
            source: Arc::clone(&state),
            dispatcher: Arc::clone(&dispatcher),
            trusted_proxies: cfg.trusted_proxies.clone(),
            allow_realip_fallback: cfg.allow_realip_fallback,
        };

        // Build a stateful sub-router for this single route, then
        // collapse to `Router<()>` via `with_state` so we can
        // merge into the parent without leaking the per-route
        // state type.
        let leaf: Router<()> = Router::new()
            .route(&s.source.path, post(handle_request))
            .layer(RequestBodyLimitLayer::new(body_cap))
            .with_state(route_state);
        router = router.merge(leaf);
    }

    Ok((
        router,
        Arc::new(RouterState {
            sources,
            trusted_proxies: cfg.trusted_proxies.clone(),
            allow_realip_fallback: cfg.allow_realip_fallback,
        }),
    ))
}

#[derive(Clone)]
struct HandlerCtx {
    source: Arc<SourceState>,
    dispatcher: Arc<dyn WebhookDispatcher>,
    trusted_proxies: Vec<ipnetwork::IpNetwork>,
    allow_realip_fallback: bool,
}

async fn handle_request(
    State(ctx): State<HandlerCtx>,
    headers: HeaderMap,
    ConnectInfo(socket): ConnectInfo<SocketAddr>,
    body: AxumBytes,
) -> impl IntoResponse {
    // Gate 3 — concurrency cap.
    let _permit = if let Some(sem) = &ctx.source.semaphore {
        match Arc::clone(sem).try_acquire_owned() {
            Ok(p) => Some(p),
            Err(_) => {
                tracing::warn!(
                    source_id = %ctx.source.source_id,
                    "concurrency cap reached"
                );
                return (StatusCode::SERVICE_UNAVAILABLE, [("Retry-After", "1")], "busy").into_response();
            }
        }
    } else {
        None
    };

    // Resolve client IP for rate-limit key + envelope.
    let xff = headers
        .get("x-forwarded-for")
        .and_then(|h| h.to_str().ok());
    let real_ip = headers.get("x-real-ip").and_then(|h| h.to_str().ok());
    let client_ip = resolve_request_client_ip(
        socket.ip(),
        ProxyHeaders {
            forwarded_for: xff,
            real_ip,
        },
        &ctx.trusted_proxies,
        ctx.allow_realip_fallback,
    );

    // Gate 4 — rate limit.
    if let Some(map) = &ctx.source.rate_limit {
        let key = ClientBucketKey {
            source_id: ctx.source.source_id.clone(),
            client_ip,
        };
        if !map.try_acquire(&key) {
            tracing::warn!(
                source_id = %ctx.source.source_id,
                client_ip = %client_ip,
                "rate-limit drop"
            );
            return (StatusCode::TOO_MANY_REQUESTS, "rate limit").into_response();
        }
    }

    // Header map → simple BTreeMap<String, String>.
    let mut header_map: HashMap<String, String> = HashMap::new();
    let mut envelope_headers: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in headers.iter() {
        if let Ok(v) = value.to_str() {
            header_map.insert(name.as_str().to_string(), v.to_string());
            envelope_headers.insert(name.as_str().to_string(), v.to_string());
        }
    }

    // Gate 5 — signature + event-kind + payload.
    let body_bytes = Bytes::from(body.to_vec());
    let handled = match ctx.source.handler.handle(&header_map, body_bytes) {
        Ok(h) => h,
        Err(reject) => {
            let (status, log_msg) = map_reject(&reject);
            tracing::warn!(
                source_id = %ctx.source.source_id,
                reason = %log_msg,
                "webhook rejected"
            );
            return (status, log_msg).into_response();
        }
    };

    // Gate 6 — dispatch.
    let topic = handled.topic.clone();
    let envelope = nexo_webhook_receiver::envelope_from_handled(
        handled,
        &envelope_headers,
        Some(client_ip),
    );
    match ctx.dispatcher.dispatch(&topic, envelope).await {
        Ok(()) => (StatusCode::NO_CONTENT, "").into_response(),
        Err(nexo_webhook_receiver::DispatchError::Broker(e)) => {
            tracing::error!(
                source_id = %ctx.source.source_id,
                error = %e,
                "broker dispatch failed"
            );
            (StatusCode::BAD_GATEWAY, "broker unavailable").into_response()
        }
        Err(nexo_webhook_receiver::DispatchError::Rejected(e)) => {
            tracing::warn!(
                source_id = %ctx.source.source_id,
                error = %e,
                "envelope rejected by dispatcher"
            );
            (StatusCode::UNPROCESSABLE_ENTITY, "rejected").into_response()
        }
        Err(other) => {
            // `DispatchError` is `#[non_exhaustive]`; future variants
            // are surfaced as a generic 502 with the error stringified.
            tracing::error!(
                source_id = %ctx.source.source_id,
                error = %other,
                "unknown dispatch error variant"
            );
            (StatusCode::BAD_GATEWAY, "dispatch failed").into_response()
        }
    }
}

fn map_reject(r: &RejectReason) -> (StatusCode, String) {
    match r {
        RejectReason::OversizedBody { .. } => (StatusCode::PAYLOAD_TOO_LARGE, r.to_string()),
        RejectReason::MissingSignatureHeader { .. }
        | RejectReason::InvalidSignature { .. } => (StatusCode::UNAUTHORIZED, r.to_string()),
        RejectReason::SecretMissing { .. } => {
            (StatusCode::INTERNAL_SERVER_ERROR, r.to_string())
        }
        RejectReason::MissingEventKind { .. }
        | RejectReason::InvalidBodyJson { .. }
        | RejectReason::InvalidEventKindForSubject { .. } => {
            (StatusCode::UNPROCESSABLE_ENTITY, r.to_string())
        }
        // `RejectReason` is `#[non_exhaustive]`; future reasons
        // default to 422 until a more specific status is wired.
        _ => (StatusCode::UNPROCESSABLE_ENTITY, r.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use nexo_webhook_receiver::{
        EventKindSource, RecordingWebhookDispatcher, SignatureAlgorithm, SignatureSpec,
        WebhookSourceConfig,
    };
    use nexo_config::types::channels::ChannelRateLimit;
    use nexo_config::types::webhook_receiver::WebhookSourceWithLimits;
    use tower::ServiceExt;

    fn mk_cfg(secret_env: &str) -> WebhookServerConfig {
        WebhookServerConfig {
            enabled: true,
            sources: vec![WebhookSourceWithLimits {
                source: WebhookSourceConfig {
                    id: "github".into(),
                    path: "/hooks/github".into(),
                    signature: SignatureSpec {
                        algorithm: SignatureAlgorithm::HmacSha256,
                        header: "X-Hub-Signature-256".into(),
                        prefix: "sha256=".into(),
                        secret_env: secret_env.into(),
                    },
                    publish_to: "webhook.github.${event_kind}".into(),
                    event_kind_from: EventKindSource::Header {
                        name: "X-GitHub-Event".into(),
                    },
                    body_cap_bytes: None,
                },
                rate_limit: None,
                concurrency_cap: None,
            }],
            ..Default::default()
        }
    }

    fn hmac_sha256_hex(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    /// Send a synthetic POST through the axum router by injecting
    /// `ConnectInfo<SocketAddr>` via request extensions. axum 0.7
    /// honours the extension when `into_make_service_with_connect_info`
    /// isn't used, so this is the canonical pattern for unit
    /// tests of routes that need `ConnectInfo`.
    async fn oneshot_with_peer(
        router: Router,
        mut req: Request<Body>,
        peer: SocketAddr,
    ) -> axum::http::Response<Body> {
        req.extensions_mut().insert(ConnectInfo(peer));
        router.oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn happy_path_via_oneshot_with_peer() {
        std::env::set_var("WEBHOOK_TEST_SECRET_PATH1", "topsecret");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_PATH1");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, _state) = build_router(&cfg, dispatcher.clone()).unwrap();

        let body = serde_json::to_vec(&serde_json::json!({"action":"opened"})).unwrap();
        let sig = format!("sha256={}", hmac_sha256_hex("topsecret", &body));
        let req = Request::builder()
            .method("POST")
            .uri("/hooks/github")
            .header("X-Hub-Signature-256", sig)
            .header("X-GitHub-Event", "pull_request")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = oneshot_with_peer(router, req, "127.0.0.1:9000".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let captured = dispatcher.captured().await;
        assert_eq!(captured.len(), 1);
        let (topic, env) = &captured[0];
        assert_eq!(topic, "webhook.github.pull_request");
        assert_eq!(env.source_id, "github");
        assert_eq!(env.event_kind, "pull_request");
        assert_eq!(env.body_json["action"], "opened");
    }

    #[tokio::test]
    async fn invalid_signature_returns_401_no_dispatch() {
        std::env::set_var("WEBHOOK_TEST_SECRET_PATH2", "topsecret");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_PATH2");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, _state) = build_router(&cfg, dispatcher.clone()).unwrap();

        let body = serde_json::to_vec(&serde_json::json!({"a":1})).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/hooks/github")
            .header("X-Hub-Signature-256", "sha256=deadbeef")
            .header("X-GitHub-Event", "push")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = oneshot_with_peer(router, req, "127.0.0.1:9000".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(dispatcher.len().await, 0);
    }

    #[tokio::test]
    async fn missing_signature_header_returns_401() {
        std::env::set_var("WEBHOOK_TEST_SECRET_PATH3", "topsecret");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_PATH3");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, _state) = build_router(&cfg, dispatcher.clone()).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/hooks/github")
            .header("X-GitHub-Event", "push")
            .body(Body::from("{}"))
            .unwrap();

        let resp = oneshot_with_peer(router, req, "127.0.0.1:9000".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn body_over_cap_returns_413() {
        std::env::set_var("WEBHOOK_TEST_SECRET_PATH4", "topsecret");
        let mut cfg = mk_cfg("WEBHOOK_TEST_SECRET_PATH4");
        cfg.body_cap_bytes = 16;
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, _state) = build_router(&cfg, dispatcher).unwrap();

        let big = vec![b'x'; 1024];
        let req = Request::builder()
            .method("POST")
            .uri("/hooks/github")
            .header("X-Hub-Signature-256", "sha256=deadbeef")
            .header("X-GitHub-Event", "push")
            .body(Body::from(big))
            .unwrap();

        let resp = oneshot_with_peer(router, req, "127.0.0.1:9000".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn missing_event_kind_header_returns_422() {
        std::env::set_var("WEBHOOK_TEST_SECRET_PATH5", "topsecret");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_PATH5");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, _state) = build_router(&cfg, dispatcher).unwrap();

        let body = serde_json::to_vec(&serde_json::json!({"a":1})).unwrap();
        let sig = format!("sha256={}", hmac_sha256_hex("topsecret", &body));
        let req = Request::builder()
            .method("POST")
            .uri("/hooks/github")
            .header("X-Hub-Signature-256", sig)
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = oneshot_with_peer(router, req, "127.0.0.1:9000".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn rate_limit_drops_excess_requests() {
        std::env::set_var("WEBHOOK_TEST_SECRET_PATH6", "topsecret");
        let mut cfg = mk_cfg("WEBHOOK_TEST_SECRET_PATH6");
        cfg.default_rate_limit = Some(ChannelRateLimit {
            rps: 0.0001,
            burst: 1,
        });
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, _state) = build_router(&cfg, dispatcher.clone()).unwrap();

        let body = serde_json::to_vec(&serde_json::json!({"a":1})).unwrap();
        let sig = format!("sha256={}", hmac_sha256_hex("topsecret", &body));
        let mk_req = || {
            Request::builder()
                .method("POST")
                .uri("/hooks/github")
                .header("X-Hub-Signature-256", sig.clone())
                .header("X-GitHub-Event", "push")
                .header("Content-Type", "application/json")
                .body(Body::from(body.clone()))
                .unwrap()
        };

        // First request consumed the only token.
        let resp1 = oneshot_with_peer(
            router.clone(),
            mk_req(),
            "127.0.0.1:9000".parse().unwrap(),
        )
        .await;
        assert_eq!(resp1.status(), StatusCode::NO_CONTENT);

        // Second request: rate-limit drop.
        let resp2 = oneshot_with_peer(router, mk_req(), "127.0.0.1:9000".parse().unwrap()).await;
        assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn build_rejects_empty_sources() {
        let cfg = WebhookServerConfig::default();
        let dispatcher = RecordingWebhookDispatcher::new();
        let result = build_router(&cfg, dispatcher);
        // Default cfg is disabled — `validate()` is Ok (skipped),
        // but `sources.is_empty()` triggers `NoSources`.
        assert!(matches!(result, Err(WebhookRouterError::NoSources)));
    }

    #[tokio::test]
    async fn router_state_lists_source_ids() {
        std::env::set_var("WEBHOOK_TEST_SECRET_PATH7", "topsecret");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_PATH7");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (_router, state) = build_router(&cfg, dispatcher).unwrap();
        assert_eq!(state.source_ids(), vec!["github"]);
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        std::env::set_var("WEBHOOK_TEST_SECRET_PATH8", "topsecret");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_PATH8");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, _state) = build_router(&cfg, dispatcher).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/no/such/path")
            .body(Body::from("{}"))
            .unwrap();
        let resp = oneshot_with_peer(router, req, "127.0.0.1:9000".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn wrong_method_returns_405() {
        std::env::set_var("WEBHOOK_TEST_SECRET_PATH9", "topsecret");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_PATH9");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, _state) = build_router(&cfg, dispatcher).unwrap();

        let req = Request::builder()
            .method("GET")
            .uri("/hooks/github")
            .body(Body::empty())
            .unwrap();
        let resp = oneshot_with_peer(router, req, "127.0.0.1:9000".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn secret_missing_returns_500() {
        // No env var → `WebhookHandler::handle` returns
        // `RejectReason::SecretMissing`.
        std::env::remove_var("WEBHOOK_TEST_SECRET_NEVER");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_NEVER");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, _state) = build_router(&cfg, dispatcher).unwrap();

        let body = serde_json::to_vec(&serde_json::json!({"a":1})).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/hooks/github")
            .header("X-Hub-Signature-256", "sha256=deadbeef")
            .header("X-GitHub-Event", "push")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = oneshot_with_peer(router, req, "127.0.0.1:9000".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
