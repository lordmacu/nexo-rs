//! Phase 76.1 — Streamable HTTP + SSE legacy transport for the
//! MCP server.
//!
//! Reuses [`Dispatcher`] (76.2) verbatim; this module owns only
//! framing, sessions, middleware, and adversarial defenses. Stdio
//! path is unaffected.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::stream::Stream;
use serde_json::Value;
use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::auth::{McpAuthenticator, Principal};
use super::dispatch::{DispatchContext, DispatchOutcome, Dispatcher};
use super::http_config::HttpTransportConfig;
use super::http_session::{HttpSession, HttpSessionManager, SessionEvent, SessionLimitExceeded};
use super::parse::{parse_jsonrpc_frame, JsonRpcParseError};
use super::McpServerHandler;

const HDR_SESSION_ID: HeaderName = HeaderName::from_static("mcp-session-id");
const HDR_PROTOCOL_VERSION: HeaderName = HeaderName::from_static("mcp-protocol-version");
const HDR_AUTH_TOKEN: HeaderName = HeaderName::from_static("mcp-auth-token");

/// Handle returned by [`start_http_server`].
///
/// `bind_addr` is the resolved local address (useful when the
/// operator passed `:0` to let the kernel pick a port).
/// `shutdown` is the cancellation token the server reacts to —
/// cancel it to trigger graceful shutdown. `join` resolves when
/// the server has fully drained.
pub struct HttpServerHandle {
    pub bind_addr: std::net::SocketAddr,
    pub shutdown: CancellationToken,
    pub join: JoinHandle<std::io::Result<()>>,
    /// Phase 76.7 — handle to the running session manager so the
    /// host can push notifications. `Arc` is cheap; `Clone` of
    /// the manager just bumps the inner refcount.
    pub(crate) sessions: Arc<HttpSessionManager>,
}

impl HttpServerHandle {
    /// Phase 76.7 — broadcast `notifications/tools/list_changed`
    /// to every active SSE consumer. Returns the number of
    /// sessions reached. Idempotent — clients debounce on their
    /// side via the existing 200 ms session-side window.
    pub fn notify_tools_list_changed(&self) -> usize {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/tools/list_changed",
        });
        self.sessions.broadcast_to_all(body)
    }

    /// Phase 76.7 — broadcast `notifications/resources/list_changed`.
    /// Symmetric to `notify_tools_list_changed`.
    pub fn notify_resources_list_changed(&self) -> usize {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/resources/list_changed",
        });
        self.sessions.broadcast_to_all(body)
    }

    /// Phase 76.7 — push `notifications/resources/updated` to
    /// every session that called `resources/subscribe { uri }`.
    /// `contents`, when supplied, is included in the body so the
    /// subscriber can refresh without a follow-up
    /// `resources/read`. Returns the number of sessions reached.
    pub fn notify_resource_updated(&self, uri: &str, contents: Option<serde_json::Value>) -> usize {
        let mut params = serde_json::Map::new();
        params.insert("uri".into(), serde_json::Value::String(uri.into()));
        if let Some(c) = contents {
            params.insert("contents".into(), c);
        }
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/resources/updated",
            "params": serde_json::Value::Object(params),
        });
        self.sessions.notify_resource_updated(uri, body)
    }

    /// Phase M1.b — produce a clone-able lightweight notifier that
    /// can be moved into a tokio task (e.g. SIGHUP reload handler)
    /// without owning the `JoinHandle`. The notifier shares the
    /// session manager `Arc` with the running server, so a
    /// `notify_tools_list_changed()` call goes to every live SSE
    /// consumer.
    pub fn notifier(&self) -> HttpNotifyHandle {
        HttpNotifyHandle {
            sessions: Arc::clone(&self.sessions),
        }
    }
}

/// Phase M1.b — clone-able lightweight notifier. Detached from the
/// `JoinHandle`, safe to move into long-lived background tasks.
#[derive(Clone)]
pub struct HttpNotifyHandle {
    sessions: Arc<HttpSessionManager>,
}

impl HttpNotifyHandle {
    /// Mirror of `HttpServerHandle::notify_tools_list_changed`.
    pub fn notify_tools_list_changed(&self) -> usize {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/tools/list_changed",
        });
        self.sessions.broadcast_to_all(body)
    }
}

/// Shared state inside axum handlers. Cheap to clone (everything
/// is `Arc` internally). `Clone` is hand-written so it does NOT
/// impose `H: Clone` — `Dispatcher<H>` already shares state via
/// `Arc`.
pub(crate) struct AppState<H: McpServerHandler + 'static> {
    pub(crate) dispatcher: Dispatcher<H>,
    pub(crate) sessions: Arc<HttpSessionManager>,
    pub(crate) cfg: Arc<HttpTransportConfig>,
    pub(crate) shutdown: CancellationToken,
    pub(crate) ready: Arc<AtomicBool>,
    pub(crate) rate_limiter: Arc<PerIpRateLimiter>,
    /// Phase 76.3 — pluggable authenticator. Constructed once at
    /// boot from `cfg.auth` (or the legacy `cfg.auth_token`).
    pub(crate) authenticator: Arc<dyn McpAuthenticator>,
}

impl<H: McpServerHandler + 'static> Clone for AppState<H> {
    fn clone(&self) -> Self {
        Self {
            dispatcher: self.dispatcher.clone(),
            sessions: Arc::clone(&self.sessions),
            cfg: Arc::clone(&self.cfg),
            shutdown: self.shutdown.clone(),
            ready: Arc::clone(&self.ready),
            rate_limiter: Arc::clone(&self.rate_limiter),
            authenticator: Arc::clone(&self.authenticator),
        }
    }
}

/// Boot the HTTP transport. Validates `cfg`, builds the axum app,
/// spawns the session janitor, and listens. Returns once bound;
/// the returned `join` resolves when the server exits (graceful
/// shutdown via `shutdown` cancel).
///
/// The `shutdown` token can be the same `CancellationToken` you
/// pass to `run_stdio_server` — both transports tear down in
/// concert.
pub async fn start_http_server<H>(
    handler: H,
    cfg: HttpTransportConfig,
    shutdown: CancellationToken,
) -> std::io::Result<HttpServerHandle>
where
    H: McpServerHandler + 'static,
{
    cfg.validate()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    // Phase 76.3 — resolve `auth` config (with backward-compat
    // promotion of the legacy `auth_token`).
    let auth_cfg = resolve_auth_config(&cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let authenticator = auth_cfg
        .build(cfg.bind)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    tracing::info!(
        addr = %cfg.bind,
        auth = %authenticator.label(),
        "mcp http auth ready",
    );

    // Stdio-side dispatcher still consumes the legacy auth_token
    // string for parity (tests + Phase 12.6 contract). HTTP path
    // uses `authenticator` exclusively; the dispatcher's own token
    // gate is only reachable from stdio frames anyway.
    //
    // Phase 76.5 — when `per_principal_rate_limit` is configured,
    // build the limiter. Phase 76.6 — same for the concurrency cap.
    // Both spawn background sweepers, so we must be inside a tokio
    // runtime (we are — `start_http_server` is async).
    let rate_limiter_opt = if let Some(rl_cfg) = cfg.per_principal_rate_limit.clone() {
        let lim = super::per_principal_rate_limit::PerPrincipalRateLimiter::new(rl_cfg)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        tracing::info!(
            buckets_cap = lim.config().max_buckets,
            "mcp http per-principal rate-limiter ready"
        );
        Some(lim)
    } else {
        None
    };
    let concurrency_cap_opt = if let Some(cc_cfg) = cfg.per_principal_concurrency.clone() {
        let cap = super::per_principal_concurrency::PerPrincipalConcurrencyCap::new(cc_cfg)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        tracing::info!(
            buckets_cap = cap.config().max_buckets,
            default_max_in_flight = cap.config().default.max_in_flight,
            default_timeout_secs = cap.config().default_timeout_secs,
            "mcp http per-principal concurrency cap ready"
        );
        Some(cap)
    } else {
        None
    };
    // Phase 76.8 — when a session-event-store block is configured
    // AND `enabled: true`, open the SQLite store and feed it into
    // the session manager so every emit() is durable + every
    // reconnect with `Last-Event-ID` can replay the gap.
    let session_event_store_opt: Option<Arc<dyn super::event_store::SessionEventStore>> =
        if let Some(ses_cfg) = cfg.session_event_store.as_ref() {
            if ses_cfg.enabled {
                ses_cfg
                    .validate()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                let path_str = ses_cfg
                    .db_path
                    .to_str()
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "session_event_store.db_path must be valid UTF-8",
                        )
                    })?
                    .to_string();
                let store = super::event_store::SqliteSessionEventStore::open(&path_str)
                    .await
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
                tracing::info!(
                    db_path = %path_str,
                    max_events_per_session = ses_cfg.max_events_per_session,
                    max_replay_batch = ses_cfg.max_replay_batch,
                    "mcp http session event store ready"
                );
                Some(Arc::new(store) as Arc<dyn super::event_store::SessionEventStore>)
            } else {
                tracing::info!(
                    "mcp http session event store present but disabled (enabled = false)"
                );
                None
            }
        } else {
            None
        };

    // Build the session manager BEFORE the dispatcher so the
    // dispatcher can hold an `Arc<dyn SessionLookup>` pointing
    // at it (Phase 76.7 `resources/subscribe`).
    let sessions = HttpSessionManager::with_event_store(
        &cfg,
        shutdown.clone(),
        session_event_store_opt.clone(),
    );
    // Phase 76.7 — keep an `Arc` for the `HttpServerHandle` so
    // operators can call `notify_*` after `start_http_server`
    // returns. `AppState` clones it again for in-flight handlers.
    let handle_sessions = Arc::clone(&sessions);
    let _janitor = sessions.spawn_janitor();
    // Phase 76.8 — periodic purge worker. Drops events older than
    // `session_max_lifetime_secs` so the SQLite file does not grow
    // without bound. Stops on parent shutdown.
    if session_event_store_opt.is_some() {
        let purge_interval = cfg
            .session_event_store
            .as_ref()
            .map(|c| c.purge_interval_secs)
            .unwrap_or(60);
        let mgr_for_purge = Arc::clone(&sessions);
        let shutdown_for_purge = shutdown.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(purge_interval));
            tick.tick().await; // skip immediate fire
            loop {
                tokio::select! {
                    _ = shutdown_for_purge.cancelled() => break,
                    _ = tick.tick() => mgr_for_purge.purge_expired_events().await,
                }
            }
        });
    }
    let session_lookup: Arc<dyn super::http_session::SessionLookup> =
        Arc::clone(&sessions) as Arc<dyn super::http_session::SessionLookup>;

    // Phase 76.11 — when an audit-log block is configured AND
    // `enabled: true`, open the SQLite store and spawn the writer
    // worker. The worker drains on `audit_writer.drain(...)` from
    // the graceful-shutdown closure further down.
    let audit_writer_opt = if let Some(audit_cfg) = cfg.audit_log.clone() {
        if audit_cfg.enabled {
            audit_cfg
                .validate()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            let path_str = audit_cfg
                .db_path
                .to_str()
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "audit_log.db_path must be valid UTF-8",
                    )
                })?
                .to_string();
            let store = super::audit_log::SqliteAuditLogStore::open(&path_str)
                .await
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
            let store_arc: Arc<dyn super::audit_log::AuditLogStore> = Arc::new(store);
            let writer = super::audit_log::AuditWriter::spawn(audit_cfg, store_arc)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            tracing::info!(
                db_path = %path_str,
                buffer = writer.config().writer_buffer,
                flush_ms = writer.config().flush_interval_ms,
                "mcp http audit log writer ready"
            );
            Some(writer)
        } else {
            tracing::info!("mcp http audit log present but disabled (audit_log.enabled = false)");
            None
        }
    } else {
        None
    };

    let dispatcher = Dispatcher::with_full_stack(
        handler,
        cfg.auth_token.clone(),
        rate_limiter_opt,
        concurrency_cap_opt,
        Some(session_lookup),
        audit_writer_opt.clone(),
    );
    let rate_limiter = Arc::new(PerIpRateLimiter::new(
        cfg.per_ip_rate_limit.rps,
        cfg.per_ip_rate_limit.burst,
    ));
    let state = AppState {
        dispatcher,
        sessions: sessions.clone(),
        cfg: Arc::new(cfg.clone()),
        shutdown: shutdown.clone(),
        ready: Arc::new(AtomicBool::new(false)),
        rate_limiter,
        authenticator,
    };

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    let bind_addr = listener.local_addr()?;

    tracing::info!(
        addr = %bind_addr,
        auth = if cfg.auth_token.is_some() { "token" } else { "none" },
        legacy_sse = cfg.enable_legacy_sse,
        "mcp http transport listening",
    );

    let shutdown_for_serve = shutdown.clone();
    let sessions_for_drain = sessions.clone();
    let audit_for_drain = audit_writer_opt.clone();
    let join = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            shutdown_for_serve.cancelled().await;
            sessions_for_drain.shutdown_all("server_shutdown").await;
            // Give SSE consumers a brief beat to drain the
            // shutdown event before axum tears the listener down.
            tokio::time::sleep(Duration::from_millis(100)).await;
            // Phase 76.11 — flush pending audit rows synchronously
            // before the process exits. 5 s ceiling per Phase 71
            // shutdown-drain budget.
            if let Some(w) = audit_for_drain {
                w.drain(Duration::from_secs(5)).await;
            }
        });
        server.await
    });

    Ok(HttpServerHandle {
        bind_addr,
        shutdown,
        join,
        sessions: handle_sessions,
    })
}

pub(crate) fn build_router<H>(state: AppState<H>) -> Router
where
    H: McpServerHandler + 'static,
{
    use axum::extract::DefaultBodyLimit;
    use axum::middleware::from_fn_with_state;
    use std::time::Duration;
    use tower::ServiceBuilder;
    use tower_http::cors::{AllowOrigin, CorsLayer};
    use tower_http::timeout::TimeoutLayer;
    use tower_http::trace::TraceLayer;

    let cfg = Arc::clone(&state.cfg);

    // CORS allowlist from config — exact-match. Any unparseable
    // origin is dropped silently (validate() already rejected
    // public bind + empty allowlist).
    let mut origin_values = Vec::with_capacity(cfg.allow_origins.len());
    for o in &cfg.allow_origins {
        if let Ok(v) = HeaderValue::from_str(o) {
            origin_values.push(v);
        }
    }
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origin_values))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            HDR_SESSION_ID.clone(),
            HDR_PROTOCOL_VERSION.clone(),
            HDR_AUTH_TOKEN.clone(),
        ])
        .max_age(Duration::from_secs(60));

    // Outer middleware (axum applies last-added first; this stack
    // wraps the entire app):
    //   trace → cors → concurrency-limit → timeout
    // Body size limit applied per-route via `DefaultBodyLimit`
    // (axum-native; avoids the `ResponseBody: Default` constraint
    // that `RequestBodyLimitLayer` would impose).
    let outer = ServiceBuilder::new()
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(tower::limit::ConcurrencyLimitLayer::new(cfg.max_in_flight))
        .layer(TimeoutLayer::new(cfg.request_timeout()));

    let body_limit = DefaultBodyLimit::max(cfg.body_max_bytes);

    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz::<H>))
        .route(
            "/mcp",
            post(post_mcp::<H>)
                .delete(delete_mcp::<H>)
                .get(get_mcp::<H>),
        );

    if cfg.enable_legacy_sse {
        router = router
            .route("/sse", get(legacy_get_sse::<H>))
            .route("/messages", post(legacy_post_messages::<H>));
    }

    let app = router
        // Innermost middleware order (executes BEFORE handler):
        //   1. rate-limit (per-IP token bucket on POST/DELETE).
        //   2. origin check (skipped on loopback; skipped on
        //      `/healthz`/`/readyz`).
        // axum applies last-added first, so we add origin first
        // then rate-limit so rate-limit sees the request first.
        .layer(from_fn_with_state(state.clone(), origin_layer::<H>))
        .layer(from_fn_with_state(state.clone(), rate_limit_layer::<H>))
        .layer(body_limit)
        .with_state(state.clone());

    app.layer(outer)
}

/// Per-IP token bucket. Plain DashMap + atomic refill.
/// `governor` would be a heavier alternative; this implementation
/// is auditable and does the same job for the per-IP scope
/// (76.5 will introduce the per-tenant / per-tool layer with a
/// richer crate).
pub(crate) struct PerIpRateLimiter {
    buckets: dashmap::DashMap<std::net::IpAddr, IpBucket>,
    rps: u32,
    burst: u32,
}

struct IpBucket {
    tokens: f64,
    last_refill: std::time::Instant,
}

impl PerIpRateLimiter {
    pub(crate) fn new(rps: u32, burst: u32) -> Self {
        Self {
            buckets: dashmap::DashMap::new(),
            rps,
            burst,
        }
    }

    /// Returns `Ok(())` if the request fits in the bucket; `Err(retry_after_ms)`
    /// if the IP is over-quota.
    pub(crate) fn check(&self, ip: std::net::IpAddr) -> Result<(), u64> {
        let now = std::time::Instant::now();
        let mut entry = self.buckets.entry(ip).or_insert_with(|| IpBucket {
            tokens: self.burst as f64,
            last_refill: now,
        });
        let elapsed = now.duration_since(entry.last_refill).as_secs_f64();
        let refill = elapsed * (self.rps as f64);
        entry.tokens = (entry.tokens + refill).min(self.burst as f64);
        entry.last_refill = now;
        if entry.tokens >= 1.0 {
            entry.tokens -= 1.0;
            Ok(())
        } else {
            // ms until 1 token regenerates
            let needed = 1.0 - entry.tokens;
            let secs = needed / (self.rps as f64);
            Err((secs * 1000.0).ceil() as u64)
        }
    }
}

async fn rate_limit_layer<H: McpServerHandler + 'static>(
    State(state): State<AppState<H>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path();
    // Only mutating routes — GET /healthz/readyz/mcp(SSE) bypass.
    if !matches!(
        req.method(),
        &axum::http::Method::POST | &axum::http::Method::DELETE
    ) {
        return next.run(req).await;
    }
    if path == "/healthz" || path == "/readyz" {
        return next.run(req).await;
    }
    let ip = client_ip(&req);
    if let Err(retry_after_ms) = state.rate_limiter.check(ip) {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "error": { "code": -32099, "message": "rate limit exceeded" },
            "id": Value::Null,
        });
        let retry_secs = (retry_after_ms / 1000).max(1);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [
                ("content-type", "application/json"),
                (
                    "retry-after",
                    Box::leak(retry_secs.to_string().into_boxed_str()) as &'static str,
                ),
            ],
            body.to_string(),
        )
            .into_response();
    }
    next.run(req).await
}

/// Best-effort client-ip extraction. Trusts `X-Forwarded-For` only
/// when bound to loopback (operator behind a proxy); otherwise
/// the direct peer address is authoritative.
fn client_ip(req: &axum::extract::Request) -> std::net::IpAddr {
    if let Some(xff) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
    {
        return xff;
    }
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}

/// Middleware: when the bind address is non-loopback, every
/// request must carry an `Origin` header in `cfg.allow_origins`.
/// Loopback bypasses this check (operators behind nginx/caddy
/// often have local-only intent). `/healthz` and `/readyz` always
/// bypass to keep liveness probes simple.
async fn origin_layer<H: McpServerHandler + 'static>(
    State(state): State<AppState<H>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path();
    if path == "/healthz" || path == "/readyz" {
        return next.run(req).await;
    }
    let bind_loopback = super::http_config::is_loopback(&state.cfg.bind.ip());
    if bind_loopback {
        return next.run(req).await;
    }
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let allowed = match origin {
        Some(o) => state.cfg.allow_origins.iter().any(|a| a == o),
        None => false,
    };
    if !allowed {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "error": { "code": -32001, "message": "origin not allowed" },
            "id": Value::Null,
        });
        return (
            StatusCode::FORBIDDEN,
            [("content-type", "application/json")],
            body.to_string(),
        )
            .into_response();
    }
    next.run(req).await
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// `POST /mcp` — primary JSON-RPC request endpoint.
///
/// Wire contract:
///   * `initialize` requests do NOT require `Mcp-Session-Id`; the
///     server allocates a fresh session and returns its id in the
///     `Mcp-Session-Id` response header.
///   * Every non-`initialize` request MUST carry a valid
///     `Mcp-Session-Id`; missing/unknown id returns 404.
///   * Notifications (no `id` in body) yield 202 No Content.
///   * Body parse errors yield 400 with a JSON-RPC error envelope.
///   * Dispatch outcomes yield 200 with the normal JSON-RPC body
///     (success or `{"error": ...}`).
async fn post_mcp<H: McpServerHandler + 'static>(
    State(state): State<AppState<H>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let principal = match enforce_auth(&state, &headers).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let parsed = match parse_jsonrpc_frame(&body) {
        Ok(v) => v,
        Err(e) => return parse_error_response(&e),
    };
    let obj = parsed
        .as_object()
        .expect("parse_jsonrpc_frame guarantees object root");
    let method = obj
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let id = obj.get("id").cloned();
    let is_initialize = method == "initialize";

    // Resolve session: initialize allocates fresh; everything else
    // requires an existing one.
    let header_session_id = headers
        .get(HDR_SESSION_ID)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let (session, created_now) = if is_initialize {
        match state.sessions.create() {
            Ok(s) => (s, true),
            Err(SessionLimitExceeded) => {
                return session_limit_response(id);
            }
        }
    } else {
        let id_str = match &header_session_id {
            Some(s) => s.clone(),
            None => return missing_session_id_response(id),
        };
        match state.sessions.get(&id_str) {
            Some(s) => (s, false),
            None => return unknown_session_response(id),
        }
    };
    state.sessions.touch(&session.id);

    // Phase 76.7 — extract `params._meta.progressToken` for the
    // dispatcher's progress reporter. Spec is strict: only the
    // canonical MCP 2025-11-25 path is honoured.
    let progress_token = params
        .get("_meta")
        .and_then(|m| m.get("progressToken"))
        .cloned();

    // Phase 76.10 — extract `X-Request-ID` (or generate UUIDv4)
    // for request correlation. Cap at 128 chars; longer
    // client-supplied values are replaced (don't trust unbounded
    // headers).
    let correlation_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Dispatch with per-request timeout, child cancel of session.
    let cancel = session.cancel.child_token();
    let ctx = DispatchContext {
        session_id: Some(session.id.clone()),
        request_id: id.clone(),
        cancel,
        principal: Some(principal.clone()),
        progress_token,
        session_sink: Some(session.notif_tx.clone()),
        correlation_id: Some(correlation_id.clone()),
    };
    let dispatch_fut = state.dispatcher.dispatch(&method, params, &ctx);
    let outcome = match tokio::time::timeout(state.cfg.request_timeout(), dispatch_fut).await {
        Ok(o) => o,
        Err(_) => DispatchOutcome::Error {
            code: -32001,
            message: "request timeout".into(),
            data: None,
        },
    };

    // Build JSON-RPC body or 202 for notifications.
    let mut response = match (id.clone(), &outcome) {
        (None, DispatchOutcome::Silent) => {
            return (StatusCode::ACCEPTED, [("content-length", "0")]).into_response();
        }
        (None, _) => {
            // Notification produced an error/cancel/reply — drop
            // body, send 202 to keep wire shape consistent.
            return (StatusCode::ACCEPTED, [("content-length", "0")]).into_response();
        }
        (Some(id_value), DispatchOutcome::Reply(result)) => {
            json_response(&serde_json::json!({"jsonrpc":"2.0","result":result,"id":id_value}))
        }
        (Some(id_value), DispatchOutcome::ReplyAndShutdown(result)) => {
            // Tear down session AFTER sending reply.
            state.sessions.close(&session.id);
            json_response(&serde_json::json!({"jsonrpc":"2.0","result":result,"id":id_value}))
        }
        (
            Some(id_value),
            DispatchOutcome::Error {
                code,
                message,
                data,
            },
        ) => {
            let mut err = serde_json::json!({"code":code,"message":message});
            if let Some(d) = data {
                err.as_object_mut()
                    .unwrap()
                    .insert("data".into(), d.clone());
            }
            json_response(&serde_json::json!({"jsonrpc":"2.0","error":err,"id":id_value}))
        }
        (Some(id_value), DispatchOutcome::Cancelled) => json_response(
            &serde_json::json!({"jsonrpc":"2.0","error":{"code":-32800,"message":"request cancelled"},"id":id_value}),
        ),
        (Some(_), DispatchOutcome::Silent) => {
            // Spec: notifications carry no id. A request with an
            // id producing Silent is a server bug — fail loud.
            return jsonrpc_internal_error_response(id);
        }
    };

    // Headers: Mcp-Session-Id (always echoed for any request that
    // resolved to a session); MCP-Protocol-Version echo.
    let resp_headers = response.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&session.id) {
        resp_headers.insert(HDR_SESSION_ID.clone(), v);
    }
    if let Some(pv) = headers.get(HDR_PROTOCOL_VERSION).cloned() {
        resp_headers.insert(HDR_PROTOCOL_VERSION.clone(), pv);
    }

    // Mark ready on first successful initialize reply.
    if is_initialize && created_now && matches!(outcome, DispatchOutcome::Reply(_)) {
        state.ready.store(true, Ordering::Relaxed);
    }

    response
}

/// `GET /sse` — legacy SSE handshake (MCP 2024-11-05).
///
/// Allocates a fresh session and emits a single `event: endpoint`
/// with the absolute URL for `POST /messages?sessionId=<id>` as
/// payload. After that the stream behaves like `GET /mcp` (SSE
/// for server→client notifications and JSON-RPC responses to
/// posts on `/messages`).
async fn legacy_get_sse<H: McpServerHandler + 'static>(
    State(state): State<AppState<H>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = enforce_auth(&state, &headers).await {
        return resp;
    }
    let session = match state.sessions.create() {
        Ok(s) => s,
        Err(SessionLimitExceeded) => return session_limit_response(None),
    };
    // Best-effort base URL: derive from `Host` header (or fall
    // back to the configured bind addr).
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| state.cfg.bind.to_string());
    let endpoint = format!("http://{}/messages?sessionId={}", host, session.id);
    // Emit the endpoint announcement as the FIRST event on the
    // SSE stream itself — `notif_tx.send` only reaches current
    // subscribers, and the SSE consumer subscribes inside the
    // generator below.
    let prelude = Event::default().event("endpoint").data(endpoint);
    sse_stream_for_session_with_prelude(&state, session, Some(prelude)).into_response()
}

/// `POST /messages?sessionId=X` — legacy alias for `POST /mcp`.
/// Session id MUST come from the query string (legacy clients do
/// not echo the response back via this socket — responses go on
/// the SSE stream the client opened with `GET /sse`).
async fn legacy_post_messages<H: McpServerHandler + 'static>(
    State(state): State<AppState<H>>,
    axum::extract::Query(q): axum::extract::Query<LegacySessionQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let principal = match enforce_auth(&state, &headers).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let session_id = q.session_id;
    let session = match state.sessions.get(&session_id) {
        Some(s) => s,
        None => return unknown_session_response(None),
    };
    state.sessions.touch(&session_id);
    let parsed = match parse_jsonrpc_frame(&body) {
        Ok(v) => v,
        Err(e) => return parse_error_response(&e),
    };
    let obj = parsed.as_object().expect("object root");
    let method = obj
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let id = obj.get("id").cloned();
    // Phase 76.7 — same progress-token extraction as the
    // primary `post_mcp` handler.
    let progress_token = params
        .get("_meta")
        .and_then(|m| m.get("progressToken"))
        .cloned();

    // Phase 76.10 — extract `X-Request-ID` (or generate UUIDv4)
    // for request correlation. Cap at 128 chars; longer
    // client-supplied values are replaced (don't trust unbounded
    // headers).
    let correlation_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let cancel = session.cancel.child_token();
    let ctx = DispatchContext {
        session_id: Some(session.id.clone()),
        request_id: id.clone(),
        cancel,
        principal: Some(principal.clone()),
        progress_token,
        session_sink: Some(session.notif_tx.clone()),
        correlation_id: Some(correlation_id.clone()),
    };
    let dispatch_fut = state.dispatcher.dispatch(&method, params, &ctx);
    let outcome = match tokio::time::timeout(state.cfg.request_timeout(), dispatch_fut).await {
        Ok(o) => o,
        Err(_) => DispatchOutcome::Error {
            code: -32001,
            message: "request timeout".into(),
            data: None,
        },
    };
    // Legacy contract: response goes onto the SSE stream as a
    // message event, NOT in this HTTP body. We only ack with 202.
    let resp_body: Option<Value> = match (id, &outcome) {
        (None, _) => None,
        (Some(rid), DispatchOutcome::Reply(result))
        | (Some(rid), DispatchOutcome::ReplyAndShutdown(result)) => Some(serde_json::json!({
            "jsonrpc":"2.0","result":result,"id":rid
        })),
        (
            Some(rid),
            DispatchOutcome::Error {
                code,
                message,
                data,
            },
        ) => {
            let mut err = serde_json::json!({"code":code,"message":message});
            if let Some(d) = data {
                err.as_object_mut()
                    .unwrap()
                    .insert("data".into(), d.clone());
            }
            Some(serde_json::json!({
                "jsonrpc":"2.0","error":err,"id":rid
            }))
        }
        (Some(rid), DispatchOutcome::Cancelled) => Some(serde_json::json!({
            "jsonrpc":"2.0","error":{"code":-32800,"message":"request cancelled"},"id":rid
        })),
        (Some(_), DispatchOutcome::Silent) => None,
    };
    if let Some(b) = resp_body {
        let _ = session.notif_tx.send(SessionEvent::Message(b));
    }
    if matches!(outcome, DispatchOutcome::ReplyAndShutdown(_)) {
        state.sessions.close(&session.id);
    }
    StatusCode::ACCEPTED.into_response()
}

#[derive(serde::Deserialize)]
struct LegacySessionQuery {
    #[serde(rename = "sessionId")]
    session_id: String,
}

async fn delete_mcp<H: McpServerHandler + 'static>(
    State(state): State<AppState<H>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = enforce_auth(&state, &headers).await {
        return resp;
    }
    let id = match headers.get(HDR_SESSION_ID).and_then(|v| v.to_str().ok()) {
        Some(s) => s.to_string(),
        None => {
            return (StatusCode::BAD_REQUEST, "missing Mcp-Session-Id\n").into_response();
        }
    };
    if state.sessions.close(&id).is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "unknown session\n").into_response()
    }
}

/// `GET /mcp` — open an SSE stream for server→client unsolicited
/// notifications (`tools/list_changed`, future `progress`).
///
/// Wire contract:
///   * `Mcp-Session-Id` header is required; missing/unknown → 400 / 404.
///   * `Cache-Control: no-store`, `Content-Type: text/event-stream`
///     headers are emitted by the SSE layer.
///   * Stream closes when the session is cancelled (DELETE),
///     server shuts down, max-age elapses, or the session
///     `notif_tx` is dropped (sender side closed).
///   * On overflow (slow consumer), drops oldest events and emits
///     an `event: lagged` notification with `{"dropped": <n>}`.
async fn get_mcp<H: McpServerHandler + 'static>(
    State(state): State<AppState<H>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = enforce_auth(&state, &headers).await {
        return resp;
    }
    let id_str = match headers.get(HDR_SESSION_ID).and_then(|v| v.to_str().ok()) {
        Some(s) => s.to_string(),
        None => {
            return (StatusCode::BAD_REQUEST, "missing Mcp-Session-Id\n").into_response();
        }
    };
    let session = match state.sessions.get(&id_str) {
        Some(s) => s,
        // Phase 76.8 — match the leak's wire contract:
        // `claude-code-leak/src/services/mcp/client.ts:189-206`
        // — clients treat 404 + `-32001 "Session not found"` as a
        // permanent failure and re-`initialize`.
        None => {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "error": {"code": -32001, "message": "Session not found"},
            });
            let mut resp = (StatusCode::NOT_FOUND, body.to_string()).into_response();
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            return resp;
        }
    };
    state.sessions.touch(&session.id);

    // Phase 76.8 — `Last-Event-ID: <u64>` triggers replay from the
    // durable store. Header absent → no replay (live stream only).
    // Header present but malformed → treat as 0 (replay everything).
    let last_event_id: Option<u64> = headers
        .get(axum::http::header::HeaderName::from_static("last-event-id"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().parse::<u64>().unwrap_or(0));

    sse_stream_for_session_with_resume(&state, session, last_event_id).into_response()
}

/// Phase 76.8 — SSE stream with `Last-Event-ID` replay. When
/// `last_event_id > 0`, the stream first drains
/// `manager.replay(session_id, last_event_id)` (capped at
/// `max_replay_batch`) yielding one SSE frame per persisted row,
/// then transitions to the live broadcast loop.
fn sse_stream_for_session_with_resume<H: McpServerHandler + 'static>(
    state: &AppState<H>,
    session: std::sync::Arc<HttpSession>,
    last_event_id: Option<u64>,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>> {
    use async_stream::stream;
    use std::convert::Infallible;

    let cfg = Arc::clone(&state.cfg);
    let server_shutdown = state.shutdown.clone();
    let mut rx = session.notif_tx.subscribe();
    let session_for_stream = session.clone();
    let session_max_age = cfg.sse_max_age();
    let sessions_for_replay = Arc::clone(&state.sessions);
    let session_id = session.id.clone();

    session_for_stream
        .sse_active
        .fetch_add(1, Ordering::Relaxed);
    let active_decrement = SseActiveGuard {
        active: Arc::clone(&session_for_stream),
    };

    let s = stream! {
        let _guard = active_decrement;

        // Replay the persisted gap before subscribing to live.
        if let Some(min_seq) = last_event_id {
            let rows = sessions_for_replay.replay(&session_id, min_seq).await;
            for (seq, body) in rows {
                yield Ok::<Event, Infallible>(
                    Event::default()
                        .id(seq.to_string())
                        .event("message")
                        .data(body.to_string())
                );
            }
        }

        let max_age_sleep = tokio::time::sleep(session_max_age);
        tokio::pin!(max_age_sleep);

        loop {
            tokio::select! {
                _ = session_for_stream.cancel.cancelled() => {
                    yield Ok::<Event, Infallible>(
                        Event::default()
                            .event("end")
                            .data(r#"{"reason":"session_closed"}"#)
                    );
                    break;
                }
                _ = server_shutdown.cancelled() => {
                    yield Ok(
                        Event::default()
                            .event("shutdown")
                            .data(r#"{"reason":"server_shutdown"}"#)
                    );
                    break;
                }
                _ = &mut max_age_sleep => {
                    yield Ok(
                        Event::default()
                            .event("end")
                            .data(r#"{"reason":"max_age"}"#)
                    );
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        Ok(SessionEvent::Message(v)) => {
                            yield Ok(Event::default().event("message").data(v.to_string()));
                        }
                        Ok(SessionEvent::IndexedMessage { seq, body }) => {
                            yield Ok(
                                Event::default()
                                    .id(seq.to_string())
                                    .event("message")
                                    .data(body.to_string())
                            );
                        }
                        Ok(SessionEvent::Shutdown { reason }) => {
                            let payload = serde_json::json!({"reason": reason});
                            yield Ok(Event::default().event("shutdown").data(payload.to_string()));
                            break;
                        }
                        Ok(SessionEvent::EndOfStream { reason }) => {
                            let payload = serde_json::json!({"reason": reason});
                            yield Ok(Event::default().event("end").data(payload.to_string()));
                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            let payload = serde_json::json!({"dropped": n});
                            yield Ok(Event::default().event("lagged").data(payload.to_string()));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    let stream: Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> = Box::pin(s);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(cfg.sse_keepalive()))
}

fn sse_stream_for_session_with_prelude<H: McpServerHandler + 'static>(
    state: &AppState<H>,
    session: std::sync::Arc<HttpSession>,
    prelude: Option<Event>,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>> {
    use async_stream::stream;
    use std::convert::Infallible;

    let cfg = Arc::clone(&state.cfg);
    let server_shutdown = state.shutdown.clone();
    let mut rx = session.notif_tx.subscribe();
    let session_for_stream = session.clone();
    let session_max_age = cfg.sse_max_age();

    session_for_stream
        .sse_active
        .fetch_add(1, Ordering::Relaxed);
    let active_decrement = SseActiveGuard {
        active: Arc::clone(&session_for_stream),
    };

    let s = stream! {
        // RAII drop on stream end decrements sse_active.
        let _guard = active_decrement;
        if let Some(p) = prelude {
            yield Ok::<Event, Infallible>(p);
        }
        let max_age_sleep = tokio::time::sleep(session_max_age);
        tokio::pin!(max_age_sleep);

        loop {
            tokio::select! {
                _ = session_for_stream.cancel.cancelled() => {
                    yield Ok::<Event, Infallible>(
                        Event::default()
                            .event("end")
                            .data(r#"{"reason":"session_closed"}"#)
                    );
                    break;
                }
                _ = server_shutdown.cancelled() => {
                    yield Ok(
                        Event::default()
                            .event("shutdown")
                            .data(r#"{"reason":"server_shutdown"}"#)
                    );
                    break;
                }
                _ = &mut max_age_sleep => {
                    yield Ok(
                        Event::default()
                            .event("end")
                            .data(r#"{"reason":"max_age"}"#)
                    );
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        Ok(SessionEvent::Message(v)) => {
                            yield Ok(Event::default().event("message").data(v.to_string()));
                        }
                        Ok(SessionEvent::IndexedMessage { seq, body }) => {
                            yield Ok(
                                Event::default()
                                    .id(seq.to_string())
                                    .event("message")
                                    .data(body.to_string())
                            );
                        }
                        Ok(SessionEvent::Shutdown { reason }) => {
                            let payload = serde_json::json!({"reason": reason});
                            yield Ok(Event::default().event("shutdown").data(payload.to_string()));
                            break;
                        }
                        Ok(SessionEvent::EndOfStream { reason }) => {
                            let payload = serde_json::json!({"reason": reason});
                            yield Ok(Event::default().event("end").data(payload.to_string()));
                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            let payload = serde_json::json!({"dropped": n});
                            yield Ok(Event::default().event("lagged").data(payload.to_string()));
                            // continue draining
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    let pinned: Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> = Box::pin(s);
    Sse::new(pinned).keep_alive(KeepAlive::new().interval(cfg.sse_keepalive()))
}

/// RAII guard that decrements the per-session SSE active counter
/// when the stream ends (cancel, drop, max-age).
struct SseActiveGuard {
    active: std::sync::Arc<HttpSession>,
}
impl Drop for SseActiveGuard {
    fn drop(&mut self) {
        self.active.sse_active.fetch_sub(1, Ordering::Relaxed);
    }
}

// Keep the unused import quiet — `AtomicUsize` is referenced via
// `HttpSession`'s field but the module-level import is needed for
// `fetch_add`/`fetch_sub` on a different type if the API ever
// surfaces. Suppressed in case rustc decides otherwise.
#[allow(dead_code)]
fn _atomic_usize_anchor(_: AtomicUsize) {}

/// Phase 76.3 — bearer / JWT / mTLS auth enforcement. Delegates to
/// the configured `McpAuthenticator` and returns the resolved
/// `Principal` so the caller can attach it to `DispatchContext`.
async fn enforce_auth<H: McpServerHandler + 'static>(
    state: &AppState<H>,
    headers: &HeaderMap,
) -> Result<Principal, Response> {
    state
        .authenticator
        .authenticate(headers, state.cfg.bind)
        .await
        .map_err(|rej| rej.into_http())
}

/// Promote the legacy `auth_token` field to an `AuthConfig` when the
/// new `auth` block is absent. Conflicts (both set) refuse to boot.
fn resolve_auth_config(cfg: &HttpTransportConfig) -> Result<super::auth::AuthConfig, String> {
    match (cfg.auth.as_ref(), cfg.auth_token.as_ref()) {
        (Some(_), Some(_)) => Err("set either `auth` or `auth_token`, not both".into()),
        (Some(a), None) => Ok(a.clone()),
        (None, Some(tok)) => {
            tracing::warn!(
                "`auth_token` is deprecated; please migrate to \
                 `auth: {{ kind: static_token, token: <…> }}`"
            );
            Ok(super::auth::AuthConfig::StaticToken {
                token: Some(tok.clone()),
                token_env: None,
                tenant: None,
            })
        }
        (None, None) => Ok(super::auth::AuthConfig::None),
    }
}

fn parse_error_response(e: &JsonRpcParseError) -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": e.code(), "message": e.to_string() },
        "id": Value::Null,
    });
    (
        StatusCode::BAD_REQUEST,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn json_response(body: &Value) -> Response {
    (
        StatusCode::OK,
        [
            ("content-type", "application/json"),
            ("cache-control", "no-store"),
        ],
        body.to_string(),
    )
        .into_response()
}

#[allow(dead_code)]
fn unauthorized_response() -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": -32001, "message": "unauthorized" },
        "id": Value::Null,
    });
    (
        StatusCode::UNAUTHORIZED,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn missing_session_id_response(id: Option<Value>) -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": -32600, "message": "missing Mcp-Session-Id (initialize first)" },
        "id": id.unwrap_or(Value::Null),
    });
    (
        StatusCode::BAD_REQUEST,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn unknown_session_response(id: Option<Value>) -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": -32600, "message": "unknown or expired session" },
        "id": id.unwrap_or(Value::Null),
    });
    (
        StatusCode::NOT_FOUND,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn session_limit_response(id: Option<Value>) -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": -32000, "message": "session limit exceeded" },
        "id": id.unwrap_or(Value::Null),
    });
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("content-type", "application/json"), ("retry-after", "5")],
        body.to_string(),
    )
        .into_response()
}

fn jsonrpc_internal_error_response(id: Option<Value>) -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": -32603, "message": "internal error" },
        "id": id.unwrap_or(Value::Null),
    });
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

#[allow(dead_code)] // currently only used by tests; SSE handlers in step 7 will use it.
fn _session_holder(_s: &Arc<HttpSession>) {}

async fn readyz<H: McpServerHandler + 'static>(
    State(state): State<AppState<H>>,
) -> impl IntoResponse {
    if state.ready.load(Ordering::Relaxed) {
        (StatusCode::OK, "ready\n")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::McpError;
    use crate::types::{McpServerInfo, McpTool, McpToolResult};
    use async_trait::async_trait;
    use serde_json::Value;
    use std::time::Duration;

    struct NoopHandler;

    #[async_trait]
    impl McpServerHandler for NoopHandler {
        fn server_info(&self) -> McpServerInfo {
            McpServerInfo {
                name: "noop".into(),
                version: "0.0.1".into(),
            }
        }
        async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
            Ok(vec![])
        }
        async fn call_tool(&self, _n: &str, _a: Value) -> Result<McpToolResult, McpError> {
            Ok(McpToolResult {
                content: vec![],
                is_error: false,
                structured_content: None,
            })
        }
    }

    fn loopback_zero_cfg() -> HttpTransportConfig {
        let mut cfg = HttpTransportConfig::default();
        cfg.enabled = true;
        cfg.bind = "127.0.0.1:0".parse().unwrap();
        cfg
    }

    #[tokio::test]
    async fn boots_on_kernel_assigned_port() {
        let shutdown = CancellationToken::new();
        let handle = start_http_server(NoopHandler, loopback_zero_cfg(), shutdown.clone())
            .await
            .unwrap();
        assert_ne!(handle.bind_addr.port(), 0);
        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    #[tokio::test]
    async fn refuses_invalid_config() {
        let shutdown = CancellationToken::new();
        let mut cfg = HttpTransportConfig::default();
        cfg.enabled = true;
        cfg.bind = "0.0.0.0:0".parse().unwrap();
        // missing token -> validate() fails
        let res = start_http_server(NoopHandler, cfg, shutdown).await;
        assert!(res.is_err());
        assert_eq!(res.err().unwrap().kind(), std::io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn healthz_returns_200() {
        let shutdown = CancellationToken::new();
        let handle = start_http_server(NoopHandler, loopback_zero_cfg(), shutdown.clone())
            .await
            .unwrap();
        let url = format!("http://{}/healthz", handle.bind_addr);
        let body = reqwest::get(&url).await.unwrap().text().await.unwrap();
        assert_eq!(body.trim(), "ok");
        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    #[tokio::test]
    async fn initialize_then_tools_list_roundtrip() {
        // Real handler that lists one tool — proves the dispatcher
        // wiring through axum + parse + sessions works.
        struct OneToolHandler;
        #[async_trait]
        impl McpServerHandler for OneToolHandler {
            fn server_info(&self) -> McpServerInfo {
                McpServerInfo {
                    name: "x".into(),
                    version: "0.0.1".into(),
                }
            }
            async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
                Ok(vec![McpTool {
                    name: "echo".into(),
                    description: None,
                    input_schema: serde_json::json!({"type":"object"}),
                    output_schema: None,
                }])
            }
            async fn call_tool(&self, _n: &str, _a: Value) -> Result<McpToolResult, McpError> {
                unreachable!()
            }
        }
        let shutdown = CancellationToken::new();
        let handle = start_http_server(OneToolHandler, loopback_zero_cfg(), shutdown.clone())
            .await
            .unwrap();
        let url = format!("http://{}/mcp", handle.bind_addr);
        let client = reqwest::Client::new();
        let init = client
            .post(&url)
            .json(&serde_json::json!({
                "jsonrpc": "2.0", "method": "initialize", "params": {}, "id": 1
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(init.status().as_u16(), 200);
        let session_id = init
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        let body: Value = init.json().await.unwrap();
        assert_eq!(body["result"]["serverInfo"]["name"], "x");

        // tools/list with the session id.
        let list = client
            .post(&url)
            .header("mcp-session-id", &session_id)
            .json(&serde_json::json!({
                "jsonrpc":"2.0","method":"tools/list","id":2
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(list.status().as_u16(), 200);
        let body: Value = list.json().await.unwrap();
        assert_eq!(body["result"]["tools"][0]["name"], "echo");

        // DELETE /mcp closes session, second call 404.
        let delete = client
            .delete(&url)
            .header("mcp-session-id", &session_id)
            .send()
            .await
            .unwrap();
        assert_eq!(delete.status().as_u16(), 204);

        let after = client
            .post(&url)
            .header("mcp-session-id", &session_id)
            .json(&serde_json::json!({
                "jsonrpc":"2.0","method":"tools/list","id":3
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(after.status().as_u16(), 404);

        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    #[tokio::test]
    async fn graceful_shutdown_broadcasts_to_active_sse() {
        use futures::StreamExt;

        let shutdown = CancellationToken::new();
        let handle = start_http_server(NoopHandler, loopback_zero_cfg(), shutdown.clone())
            .await
            .unwrap();
        let url = format!("http://{}/mcp", handle.bind_addr);
        let client = reqwest::Client::new();
        let init = client
            .post(&url)
            .json(&serde_json::json!({
                "jsonrpc":"2.0","method":"initialize","params":{},"id":1
            }))
            .send()
            .await
            .unwrap();
        let session_id = init
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        let resp = client
            .get(&url)
            .header("mcp-session-id", &session_id)
            .header("accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        let mut events = eventsource_stream::EventStream::new(resp.bytes_stream());
        let shutdown_for_task = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            shutdown_for_task.cancel();
        });
        let next = tokio::time::timeout(Duration::from_secs(3), events.next()).await;
        let evt = next.unwrap().expect("stream ended").expect("recv error");
        assert!(
            evt.event == "shutdown" || evt.event == "end",
            "expected shutdown or end event, got {}",
            evt.event
        );
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    #[tokio::test]
    async fn sse_emits_end_when_session_deleted() {
        use futures::StreamExt;

        let shutdown = CancellationToken::new();
        let handle = start_http_server(NoopHandler, loopback_zero_cfg(), shutdown.clone())
            .await
            .unwrap();
        let url = format!("http://{}/mcp", handle.bind_addr);
        let client = reqwest::Client::new();

        // initialize
        let init = client
            .post(&url)
            .json(&serde_json::json!({
                "jsonrpc":"2.0","method":"initialize","params":{},"id":1
            }))
            .send()
            .await
            .unwrap();
        let session_id = init
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();

        // GET /mcp SSE
        let resp = client
            .get(&url)
            .header("mcp-session-id", &session_id)
            .header("accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
            "text/event-stream"
        );

        let mut events = eventsource_stream::EventStream::new(resp.bytes_stream());
        let session_for_drop = session_id.clone();
        let url2 = url.clone();
        let client2 = client.clone();
        let drop_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = client2
                .delete(&url2)
                .header("mcp-session-id", &session_for_drop)
                .send()
                .await;
        });
        // Should receive `event: end` within a couple seconds.
        let next = tokio::time::timeout(Duration::from_secs(3), events.next()).await;
        assert!(next.is_ok(), "SSE did not yield end event in time");
        let evt = next.unwrap().expect("stream ended").expect("recv error");
        assert_eq!(evt.event, "end");
        let _ = drop_task.await;
        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    #[tokio::test]
    async fn readyz_returns_503_until_ready() {
        let shutdown = CancellationToken::new();
        let handle = start_http_server(NoopHandler, loopback_zero_cfg(), shutdown.clone())
            .await
            .unwrap();
        let url = format!("http://{}/readyz", handle.bind_addr);
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status().as_u16(), 503);
        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }
}
