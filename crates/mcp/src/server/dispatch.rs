//! Phase 76.2 — transport-agnostic JSON-RPC dispatcher for the MCP
//! server.
//!
//! Owns the protocol logic; transports own framing. Built once per
//! server, cloned cheaply per request via internal `Arc`. Two
//! robustness invariants are enforced here so transports don't have
//! to repeat them:
//!
//!  * **Panic-safety.** Every call into the user-supplied
//!    `McpServerHandler` runs inside `AssertUnwindSafe(...).catch_unwind()`.
//!    A panic in handler code is converted to JSON-RPC error
//!    `-32603` and the dispatcher continues. The handler is itself
//!    responsible for not poisoning its own state across panics
//!    (use RAII / avoid `Mutex`-poisoning patterns inside handlers).
//!  * **Cooperative cancellation.** `DispatchContext::cancel` is
//!    polled alongside the dispatch future via `tokio::select!`.
//!    When it fires the dispatch resolves to
//!    `DispatchOutcome::Cancelled` and any in-flight handler future
//!    is dropped at its next `.await` suspension point.

use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::errors::McpError;
use crate::protocol::{is_supported_protocol_version, PROTOCOL_VERSION};
use crate::server::McpServerHandler;
use crate::McpTool;

/// Shared, cheaply-clonable JSON-RPC dispatcher. Clones share state
/// via `Arc` so multiple in-flight requests (HTTP, 76.1) can use the
/// same handler concurrently without re-instantiating it. The
/// `Clone` impl is hand-written so it does NOT impose `H: Clone` —
/// Walk the tool's `input_schema` to find the `enum` values for
/// `argument_name`, if any. Returns `None` for missing/absent
/// enums or any parse failure — the MCP spec treats empty as
/// "no suggestions available."
fn extract_completion_values(tools: &[McpTool], params: &Value) -> Option<Vec<String>> {
    let tool_name = params
        .get("ref")
        .and_then(|r| r.get("name"))
        .and_then(|v| v.as_str())?;
    let arg_name = params
        .get("argument")
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())?;
    let tool = tools.iter().find(|t| t.name == tool_name)?;
    let props = tool.input_schema.get("properties")?;
    let prop = props.get(arg_name)?;
    let enm = prop.get("enum")?.as_array()?;
    Some(
        enm.iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => Some(v.to_string()),
            })
            .collect(),
    )
}

/// only the `Arc` is bumped.
pub struct Dispatcher<H: McpServerHandler + 'static> {
    inner: Arc<DispatcherInner<H>>,
}

impl<H: McpServerHandler + 'static> Clone for Dispatcher<H> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct DispatcherInner<H: McpServerHandler> {
    handler: H,
    auth_token: Option<String>,
    /// Phase 76.5 — per-(tenant, tool) rate limiter. `None`
    /// disables enforcement (no overhead in the hot path).
    rate_limiter: Option<Arc<crate::server::per_principal_rate_limit::PerPrincipalRateLimiter>>,
    /// Phase 76.6 — per-(tenant, tool) concurrency cap +
    /// per-call timeout. `None` disables enforcement.
    concurrency_cap:
        Option<Arc<crate::server::per_principal_concurrency::PerPrincipalConcurrencyCap>>,
    /// Phase 76.7 — abstract session lookup so `resources/subscribe`
    /// can persist subscriptions in the HTTP session manager. Stdio
    /// passes `None` and the corresponding methods reply
    /// `Reply({})` without state.
    session_lookup: Option<Arc<dyn crate::server::http_session::SessionLookup>>,
    /// Phase 76.11 — per-call audit log writer. `None` disables
    /// audit; otherwise the dispatcher emits one `AuditRow` per
    /// dispatched method.
    audit_writer: Option<Arc<crate::server::audit_log::AuditWriter>>,
}

/// Per-request dispatch context. Built fresh by the transport for
/// each inbound JSON-RPC request.
#[derive(Debug, Clone)]
pub struct DispatchContext {
    /// Logical session this request belongs to. `None` for stdio
    /// (single implicit session). HTTP (76.1) populates this with
    /// the `Mcp-Session-Id` header.
    pub session_id: Option<String>,
    /// JSON-RPC `id` of the request, if any. Used only for tracing
    /// / structured logging — the transport handles wire wrapping.
    pub request_id: Option<Value>,
    /// Cancel signal. When this fires mid-dispatch the outcome is
    /// `Cancelled`. Conventionally this is a child token of the
    /// session-level cancel, which is in turn a child of the
    /// process-level shutdown token.
    pub cancel: CancellationToken,
    /// Phase 76.3 — caller identity. `None` only in
    /// `DispatchContext::empty()` test helpers. HTTP populates from
    /// `McpAuthenticator`; stdio populates `Principal::stdio_local()`.
    /// The dispatcher itself does NOT consume this in 76.3 — it is
    /// prepared for 76.4 multi-tenant isolation.
    pub principal: Option<crate::server::auth::Principal>,
    /// Phase 76.7 — `params._meta.progressToken` echoed from the
    /// originating `tools/call` request. When present (and the
    /// transport supplied a `session_sink`), the dispatcher hands
    /// the tool a live `ProgressReporter`; otherwise the reporter
    /// is a noop.
    pub progress_token: Option<serde_json::Value>,
    /// Phase 76.7 — broadcast sink to the originating session for
    /// server→client notifications (`notifications/progress`,
    /// future `progress`-style events). HTTP populates this from
    /// the session's `notif_tx`; stdio passes `None` (single
    /// implicit session — stdio progress emission is deferred).
    pub session_sink:
        Option<tokio::sync::broadcast::Sender<crate::server::http_session::SessionEvent>>,
    /// Phase 76.10 — request correlation id. HTTP transport
    /// extracts from the `X-Request-ID` header (or generates
    /// UUIDv4 when absent) and echoes back in the response
    /// header. Logged on every dispatch span. Capped at 128
    /// chars; longer client-supplied values get replaced with a
    /// fresh UUIDv4 (don't trust unbounded headers).
    pub correlation_id: Option<String>,
}

impl DispatchContext {
    /// Stand-alone context with a fresh, never-cancelled token and
    /// a stdio-local principal. Useful for unit tests and one-shot
    /// dispatches that have no surrounding session. Phase 76.4
    /// flipped the principal default from `None` to
    /// `Some(stdio_local)` so [`tenant`] is always callable.
    pub fn empty() -> Self {
        Self {
            session_id: None,
            request_id: None,
            cancel: CancellationToken::new(),
            principal: Some(crate::server::auth::Principal::stdio_local()),
            progress_token: None,
            session_sink: None,
            correlation_id: None,
        }
    }

    /// Phase 76.4 — borrow the caller's tenant. Panics if
    /// `principal` is `None`, which is a programming bug at the
    /// transport site (every transport in the tree populates the
    /// principal). Tests must use [`empty`] (which populates a
    /// stdio-local principal) or build a `DispatchContext` literal
    /// with `principal: Some(...)`.
    pub fn tenant(&self) -> &crate::server::auth::TenantId {
        &self
            .principal
            .as_ref()
            .expect(
                "DispatchContext.principal is None — transport must populate \
                 it (Phase 76.4 contract)",
            )
            .tenant
    }
}

/// Outcome of a single `Dispatcher::dispatch` call. The transport
/// decides how to render each variant on the wire.
#[derive(Debug)]
pub enum DispatchOutcome {
    /// Successful response; transport wraps as `{"jsonrpc","result","id"}`.
    Reply(Value),
    /// Successful response that should also tear down the session
    /// after sending. Used for the `shutdown` method.
    ReplyAndShutdown(Value),
    /// Protocol or handler error; transport wraps as
    /// `{"jsonrpc","error":{"code","message","data"?},"id"}`.
    /// The `data` field is optional structured detail attached to
    /// the error — Phase 76.5 uses it for `retry_after_ms` on
    /// rate-limit rejections; future phases (76.7 progress,
    /// 76.11 audit) may grow other shapes.
    Error {
        code: i32,
        message: String,
        #[allow(dead_code)]
        data: Option<Value>,
    },
    /// Notification was processed; transport MUST NOT emit a
    /// response (no `id` on the wire).
    Silent,
    /// Dispatch aborted because `ctx.cancel` fired. Transport emits
    /// `-32800 request cancelled` if the request had an `id`,
    /// otherwise drops it silently.
    Cancelled,
}

impl<H: McpServerHandler + 'static> Dispatcher<H> {
    /// Build a dispatcher.
    ///
    /// `auth_token`, when `Some`, is required in the `initialize`
    /// request as either `params.auth_token` or
    /// `params._meta.auth_token`. The check uses a constant-time
    /// comparison (see [`consteq`]).
    pub fn new(handler: H, auth_token: Option<String>) -> Self {
        Self {
            inner: Arc::new(DispatcherInner {
                handler,
                auth_token,
                rate_limiter: None,
                concurrency_cap: None,
                session_lookup: None,
                audit_writer: None,
            }),
        }
    }

    /// Phase 76.5 — build a dispatcher with a per-principal
    /// rate-limiter attached. The limiter applies only to
    /// `tools/call`; `initialize`, `tools/list`, `shutdown`, etc.
    /// bypass. Stdio principals bypass too.
    pub fn with_rate_limiter(
        handler: H,
        auth_token: Option<String>,
        rate_limiter: Arc<crate::server::per_principal_rate_limit::PerPrincipalRateLimiter>,
    ) -> Self {
        Self {
            inner: Arc::new(DispatcherInner {
                handler,
                auth_token,
                rate_limiter: Some(rate_limiter),
                concurrency_cap: None,
                session_lookup: None,
                audit_writer: None,
            }),
        }
    }

    /// Phase 76.6 — build a dispatcher with both layers attached.
    /// Either or both may be `None`. Order at runtime: rate-limit
    /// gate (76.5) → concurrency cap acquire (76.6) → handler call
    /// wrapped in `tokio::time::timeout`. Permits are RAII-released
    /// on success, error, timeout, OR cancellation.
    pub fn with_rate_and_concurrency(
        handler: H,
        auth_token: Option<String>,
        rate_limiter: Option<Arc<crate::server::per_principal_rate_limit::PerPrincipalRateLimiter>>,
        concurrency_cap: Option<
            Arc<crate::server::per_principal_concurrency::PerPrincipalConcurrencyCap>,
        >,
    ) -> Self {
        Self {
            inner: Arc::new(DispatcherInner {
                handler,
                auth_token,
                rate_limiter,
                concurrency_cap,
                session_lookup: None,
                audit_writer: None,
            }),
        }
    }

    /// Phase 76.7 — full constructor with session-lookup hook so
    /// `resources/subscribe` can persist per-session state.
    pub fn with_rate_concurrency_and_sessions(
        handler: H,
        auth_token: Option<String>,
        rate_limiter: Option<Arc<crate::server::per_principal_rate_limit::PerPrincipalRateLimiter>>,
        concurrency_cap: Option<
            Arc<crate::server::per_principal_concurrency::PerPrincipalConcurrencyCap>,
        >,
        session_lookup: Option<Arc<dyn crate::server::http_session::SessionLookup>>,
    ) -> Self {
        Self {
            inner: Arc::new(DispatcherInner {
                handler,
                auth_token,
                rate_limiter,
                concurrency_cap,
                session_lookup,
                audit_writer: None,
            }),
        }
    }

    /// Phase 76.11 — full constructor with the audit writer.
    /// Mirrors `with_rate_concurrency_and_sessions` and adds the
    /// last-mile durable trail. Each subsequent phase may add a
    /// new constructor; older ones stay compatible by setting the
    /// new field to `None`.
    pub fn with_full_stack(
        handler: H,
        auth_token: Option<String>,
        rate_limiter: Option<Arc<crate::server::per_principal_rate_limit::PerPrincipalRateLimiter>>,
        concurrency_cap: Option<
            Arc<crate::server::per_principal_concurrency::PerPrincipalConcurrencyCap>,
        >,
        session_lookup: Option<Arc<dyn crate::server::http_session::SessionLookup>>,
        audit_writer: Option<Arc<crate::server::audit_log::AuditWriter>>,
    ) -> Self {
        Self {
            inner: Arc::new(DispatcherInner {
                handler,
                auth_token,
                rate_limiter,
                concurrency_cap,
                session_lookup,
                audit_writer,
            }),
        }
    }

    /// Configured initialize-time auth token, if any. Transports
    /// may use this for header-level pre-auth (HTTP, 76.1) before
    /// the dispatcher sees the request.
    pub fn auth_token(&self) -> Option<&str> {
        self.inner.auth_token.as_deref()
    }

    /// Borrow the underlying handler (read-only).
    pub fn handler(&self) -> &H {
        &self.inner.handler
    }

    /// Dispatch one JSON-RPC request. See module docs for the
    /// panic-safety and cancellation invariants.
    pub async fn dispatch(
        &self,
        method: &str,
        params: Value,
        ctx: &DispatchContext,
    ) -> DispatchOutcome {
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => DispatchOutcome::Cancelled,
            outcome = self.do_dispatch(method, params, ctx) => outcome,
        }
    }

    async fn do_dispatch(
        &self,
        method: &str,
        params: Value,
        ctx: &DispatchContext,
    ) -> DispatchOutcome {
        let handler = &self.inner.handler;
        let expected_auth_token = self.inner.auth_token.as_deref();

        match method {
            "initialize" => {
                if let Some(expected) = expected_auth_token {
                    let provided = extract_auth_token(&params).unwrap_or("");
                    if !consteq(provided, expected) {
                        tracing::warn!("mcp initialize rejected: invalid auth token");
                        return DispatchOutcome::Error {
                            code: -32001,
                            message: "unauthorized initialize".into(),
                            data: None,
                        };
                    }
                }
                let client_info = params.get("clientInfo");
                let client_name = client_info
                    .and_then(|c| c.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>");
                let client_version = client_info
                    .and_then(|c| c.get("version"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>");
                // Phase 73 — echo the client's requested protocol
                // version when supported. Claude Code 2.1+ sends
                // `2025-11-25`; replying with our hardcoded
                // `2024-11-05` made Claude treat the server as
                // protocol-mismatched and drop every announced
                // tool. Negotiation rule: echo the client's choice
                // when we know it, fall back to ours otherwise.
                let client_protocol_version =
                    params.get("protocolVersion").and_then(|v| v.as_str());
                let agreed_version = match client_protocol_version {
                    Some(v) if is_supported_protocol_version(v) => v,
                    _ => PROTOCOL_VERSION,
                };
                tracing::info!(
                    client_name,
                    client_version,
                    client_protocol_version = client_protocol_version.unwrap_or("<missing>"),
                    agreed_protocol_version = agreed_version,
                    "mcp client connected",
                );
                DispatchOutcome::Reply(serde_json::json!({
                    "protocolVersion": agreed_version,
                    "capabilities": handler.capabilities(),
                    "serverInfo": handler.server_info(),
                }))
            }
            "notifications/initialized" | "notifications/cancelled" => {
                tracing::debug!(method, "mcp notification");
                DispatchOutcome::Silent
            }
            "ping" => DispatchOutcome::Reply(serde_json::json!({})),
            "shutdown" => DispatchOutcome::ReplyAndShutdown(Value::Null),
            "completion/complete" => {
                tracing::debug!("mcp completion/complete");
                let values = match handler.list_tools().await {
                    Ok(tools) => extract_completion_values(&tools, &params)
                        .unwrap_or_default(),
                    Err(e) => {
                        tracing::debug!(error = %e, "completion/complete list_tools failed");
                        Vec::new()
                    }
                };
                DispatchOutcome::Reply(serde_json::json!({
                    "completion": {
                        "values": values,
                        "total": values.len(),
                        "hasMore": false
                    }
                }))
            }
            "tools/list" => {
                guarded(handler.list_tools(), "tools/list", |tools| {
                    tracing::debug!(count = tools.len(), "mcp tools/list");
                    // Phase 73 — omit `nextCursor` when there is no
                    // next page. Returning `nextCursor: null` made
                    // Claude Code 2.1's schema validator refuse the
                    // tool list, surfacing as
                    // "Available MCP tools: none" while the
                    // connection log claimed `connected`.
                    DispatchOutcome::Reply(serde_json::json!({ "tools": tools }))
                })
                .await
            }
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                if name.is_empty() {
                    tracing::warn!("mcp tools/call missing 'name'");
                    return DispatchOutcome::Error {
                        code: -32602,
                        message: "tools/call missing 'name'".into(),
                        data: None,
                    };
                }
                let bypass_stdio = ctx
                    .principal
                    .as_ref()
                    .map(|p| matches!(p.auth_method, crate::server::auth::AuthMethod::Stdio))
                    .unwrap_or(false);
                let tenant_opt = ctx.principal.as_ref().map(|p| &p.tenant);

                // Phase 76.10 — RAII gauge guard. Increments
                // `mcp_in_flight{tenant,tool}` now; the Drop on
                // every exit path (success / error / timeout /
                // cancel / panic) decrements it.
                let metrics_tenant = tenant_opt
                    .map(|t| t.as_str().to_string())
                    .unwrap_or_else(|| "-".to_string());
                let _in_flight_guard = if !bypass_stdio {
                    Some(crate::server::telemetry::InFlightGuard::new(
                        &metrics_tenant,
                        name,
                    ))
                } else {
                    None
                };
                let started_total = std::time::Instant::now();
                let metrics_tool = name.to_string();

                // Phase 76.5 — per-(tenant, tool) rate-limit gate.
                // Stdio principals bypass (single-tenant by
                // construction). When the limiter is `None` (not
                // configured) this branch compiles to a single
                // `Option::is_some` test.
                if !bypass_stdio {
                    if let (Some(limiter), Some(tenant)) =
                        (self.inner.rate_limiter.as_ref(), tenant_opt)
                    {
                        if let Err(hit) = limiter.check(tenant, name) {
                            crate::server::telemetry::bump_rate_limit_hit(
                                &metrics_tenant,
                                &metrics_tool,
                            );
                            crate::server::telemetry::bump_request(
                                &metrics_tenant,
                                &metrics_tool,
                                crate::server::telemetry::Outcome::RateLimited,
                            );
                            return DispatchOutcome::Error {
                                code: -32099,
                                message: "rate limit exceeded".into(),
                                data: Some(serde_json::json!({
                                    "retry_after_ms": hit.retry_after_ms
                                })),
                            };
                        }
                    }
                }

                // Phase 76.6 — per-(tenant, tool) concurrency cap +
                // per-call timeout. Stdio bypasses (single-tenant);
                // when `concurrency_cap` is None we just default to
                // a 30 s timeout from `MAX_REQUEST_TIMEOUT_SECS`-ish
                // semantics — but we still apply the timeout so a
                // stuck handler never holds the dispatcher forever.
                let permit_timeout = if !bypass_stdio {
                    if let (Some(cap), Some(tenant)) =
                        (self.inner.concurrency_cap.as_ref(), tenant_opt)
                    {
                        match cap.acquire(tenant, name, &ctx.cancel).await {
                            Ok(permit) => Some((Some(permit), cap.timeout_for(name))),
                            Err(crate::server::per_principal_concurrency::AcquireRejection::Cancelled) => {
                                crate::server::telemetry::bump_request(
                                    &metrics_tenant,
                                    &metrics_tool,
                                    crate::server::telemetry::Outcome::Cancelled,
                                );
                                return DispatchOutcome::Cancelled;
                            }
                            Err(crate::server::per_principal_concurrency::AcquireRejection::QueueTimeout {
                                max_in_flight,
                                queue_wait_ms,
                            }) => {
                                crate::server::telemetry::bump_concurrency_rejection(
                                    &metrics_tenant,
                                    &metrics_tool,
                                );
                                crate::server::telemetry::bump_request(
                                    &metrics_tenant,
                                    &metrics_tool,
                                    crate::server::telemetry::Outcome::Denied,
                                );
                                return DispatchOutcome::Error {
                                    code: -32002,
                                    message: "concurrent calls exceeded".into(),
                                    data: Some(serde_json::json!({
                                        "max_in_flight": max_in_flight,
                                        "queue_wait_ms_exceeded": queue_wait_ms,
                                    })),
                                };
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Phase 76.7 — build the progress reporter from
                // the request's `params._meta.progressToken` and
                // the session's broadcast sink. When either is
                // absent the reporter is a noop (cheap: no
                // allocation in the hot path).
                let progress = match (&ctx.progress_token, &ctx.session_sink) {
                    (Some(token), Some(sink)) => crate::server::progress::ProgressReporter::new(
                        token.clone(),
                        sink.clone(),
                        std::time::Duration::from_millis(20),
                    ),
                    _ => crate::server::progress::ProgressReporter::noop(),
                };

                let started = std::time::Instant::now();
                let name_owned = name.to_string();
                let dispatch_ctx = ctx.clone();
                let handler_fut = guarded(
                    handler.call_tool_streaming_with_context(name, args, progress, &dispatch_ctx),
                    "tools/call",
                    move |result| {
                        let duration_ms = started.elapsed().as_millis() as u64;
                        tracing::info!(
                            tool = %name_owned,
                            duration_ms,
                            is_error = result.is_error,
                            "mcp tool call"
                        );
                        match serde_json::to_value(result) {
                            Ok(v) => DispatchOutcome::Reply(v),
                            Err(e) => DispatchOutcome::Error {
                                code: -32603,
                                message: format!("encode result: {e}"),
                                data: None,
                            },
                        }
                    },
                );

                let outcome = if let Some((permit, timeout_dur)) = permit_timeout {
                    let _permit = permit; // RAII; dropped at end of scope.
                    match tokio::time::timeout(timeout_dur, handler_fut).await {
                        Ok(out) => out,
                        Err(_elapsed) => {
                            if let Some(cap) = self.inner.concurrency_cap.as_ref() {
                                cap.note_timeout();
                            }
                            crate::server::telemetry::bump_timeout(&metrics_tenant, &metrics_tool);
                            DispatchOutcome::Error {
                                code: -32001,
                                message: "request timeout".into(),
                                data: Some(serde_json::json!({
                                    "timeout_ms": timeout_dur.as_millis() as u64
                                })),
                            }
                        }
                    }
                } else {
                    handler_fut.await
                };

                // Phase 76.10 — observe outcome + duration. The
                // InFlightGuard drops at scope end (decrements
                // gauge regardless of branch).
                let outcome_label = match &outcome {
                    DispatchOutcome::Reply(_) | DispatchOutcome::ReplyAndShutdown(_) => {
                        crate::server::telemetry::Outcome::Ok
                    }
                    DispatchOutcome::Error { code: -32001, .. } => {
                        crate::server::telemetry::Outcome::Timeout
                    }
                    DispatchOutcome::Error { code: -32603, .. } => {
                        crate::server::telemetry::Outcome::Panicked
                    }
                    DispatchOutcome::Error { .. } => crate::server::telemetry::Outcome::Error,
                    DispatchOutcome::Cancelled => crate::server::telemetry::Outcome::Cancelled,
                    DispatchOutcome::Silent => crate::server::telemetry::Outcome::Ok,
                };
                crate::server::telemetry::bump_request(
                    &metrics_tenant,
                    &metrics_tool,
                    outcome_label,
                );
                crate::server::telemetry::observe_request_duration(
                    &metrics_tenant,
                    &metrics_tool,
                    started_total.elapsed(),
                );

                // Phase 76.11 — emit one audit row per
                // tools/call dispatch. Non-blocking try_send;
                // drops counted internally.
                if let Some(writer) = self.inner.audit_writer.as_ref() {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    let started_ms = now_ms - started_total.elapsed().as_millis() as i64;
                    let principal = ctx.principal.as_ref();
                    let tenant_str = principal
                        .map(|p| p.tenant.as_str().to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let subject = principal.map(|p| p.subject.clone());
                    let auth_method = principal
                        .map(|p| {
                            match p.auth_method {
                                crate::server::auth::AuthMethod::Stdio => "stdio",
                                crate::server::auth::AuthMethod::StaticToken => "static_token",
                                crate::server::auth::AuthMethod::Jwt => "jwt",
                                crate::server::auth::AuthMethod::MutualTls => "mutual_tls",
                                crate::server::auth::AuthMethod::None => "none",
                            }
                            .to_string()
                        })
                        .unwrap_or_else(|| "none".to_string());

                    let (error_code, error_message, retry_after_ms) = match &outcome {
                        DispatchOutcome::Error {
                            code,
                            message,
                            data,
                        } => (
                            Some(*code),
                            Some({
                                let mut m = message.clone();
                                m.truncate(512);
                                m
                            }),
                            data.as_ref()
                                .and_then(|d| d.get("retry_after_ms"))
                                .and_then(|v| v.as_i64()),
                        ),
                        _ => (None, None, None),
                    };
                    let row = crate::server::audit_log::AuditRow {
                        call_id: uuid::Uuid::new_v4().to_string(),
                        request_id: ctx.correlation_id.clone(),
                        session_id: ctx.session_id.clone(),
                        tenant: tenant_str,
                        subject,
                        auth_method,
                        method: "tools/call".to_string(),
                        tool_name: Some(metrics_tool.clone()),
                        args_hash: None, // Phase 76.11 follow-up: hash params.arguments
                        args_size_bytes: 0,
                        started_at_ms: started_ms,
                        completed_at_ms: Some(now_ms),
                        duration_ms: Some(started_total.elapsed().as_millis() as i64),
                        outcome: outcome_label,
                        error_code,
                        error_message,
                        result_size_bytes: None,
                        retry_after_ms,
                    };
                    writer.try_send(row);
                }

                outcome
            }
            "resources/list" => {
                guarded(handler.list_resources(), "resources/list", |resources| {
                    tracing::debug!(count = resources.len(), "mcp resources/list");
                    DispatchOutcome::Reply(serde_json::json!({
                        "resources": resources,
                        "nextCursor": Value::Null,
                    }))
                })
                .await
            }
            "resources/read" => {
                let uri = params.get("uri").and_then(|n| n.as_str()).unwrap_or("");
                if uri.is_empty() {
                    tracing::warn!("mcp resources/read missing 'uri'");
                    return DispatchOutcome::Error {
                        code: -32602,
                        message: "resources/read missing 'uri'".into(),
                        data: None,
                    };
                }
                guarded(handler.read_resource(uri), "resources/read", |contents| {
                    DispatchOutcome::Reply(serde_json::json!({
                        "contents": contents,
                    }))
                })
                .await
            }
            "resources/subscribe" => {
                // Phase 76.7 — record this session's interest in
                // a URI. When `session_lookup` is None (stdio), we
                // accept the request silently — there's no
                // session manager to mutate.
                let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                if uri.is_empty() {
                    return DispatchOutcome::Error {
                        code: -32602,
                        message: "resources/subscribe missing 'uri'".into(),
                        data: None,
                    };
                }
                if let (Some(lookup), Some(session_id)) = (
                    self.inner.session_lookup.as_ref(),
                    ctx.session_id.as_deref(),
                ) {
                    let _ = lookup.subscribe(session_id, uri);
                }
                DispatchOutcome::Reply(serde_json::json!({}))
            }
            "resources/unsubscribe" => {
                let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                if uri.is_empty() {
                    return DispatchOutcome::Error {
                        code: -32602,
                        message: "resources/unsubscribe missing 'uri'".into(),
                        data: None,
                    };
                }
                if let (Some(lookup), Some(session_id)) = (
                    self.inner.session_lookup.as_ref(),
                    ctx.session_id.as_deref(),
                ) {
                    let _ = lookup.unsubscribe(session_id, uri);
                }
                DispatchOutcome::Reply(serde_json::json!({}))
            }
            "resources/templates/list" => {
                guarded(
                    handler.list_resource_templates(),
                    "resources/templates/list",
                    |templates| {
                        DispatchOutcome::Reply(serde_json::json!({
                            "resourceTemplates": templates,
                            "nextCursor": Value::Null,
                        }))
                    },
                )
                .await
            }
            "prompts/list" => {
                guarded(handler.list_prompts(), "prompts/list", |prompts| {
                    DispatchOutcome::Reply(serde_json::json!({
                        "prompts": prompts,
                        "nextCursor": Value::Null,
                    }))
                })
                .await
            }
            "prompts/get" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name.is_empty() {
                    tracing::warn!("mcp prompts/get missing 'name'");
                    return DispatchOutcome::Error {
                        code: -32602,
                        message: "prompts/get missing 'name'".into(),
                        data: None,
                    };
                }
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                guarded(handler.get_prompt(name, args), "prompts/get", |result| {
                    match serde_json::to_value(result) {
                        Ok(v) => DispatchOutcome::Reply(v),
                        Err(e) => DispatchOutcome::Error {
                            code: -32603,
                            message: format!("encode result: {e}"),
                            data: None,
                        },
                    }
                })
                .await
            }
            _ => {
                tracing::warn!(method, "mcp unknown method");
                DispatchOutcome::Error {
                    code: -32601,
                    message: format!("method not found: {method}"),
                    data: None,
                }
            }
        }
    }
}

/// Run a handler future under panic-and-error guards. A handler
/// `Err(McpError::Protocol)` becomes `-32602`; any other error
/// becomes `-32000`; a panic becomes `-32603` with a generic
/// message (the panic payload is logged at `error` level but never
/// returned on the wire).
async fn guarded<F, T, M>(fut: F, op: &'static str, on_ok: M) -> DispatchOutcome
where
    F: Future<Output = Result<T, McpError>>,
    M: FnOnce(T) -> DispatchOutcome,
{
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(Ok(value)) => on_ok(value),
        Ok(Err(McpError::Protocol(msg))) => {
            tracing::warn!(op, error = %msg, "mcp handler protocol error");
            DispatchOutcome::Error {
                code: -32602,
                message: msg,
                data: None,
            }
        }
        Ok(Err(other)) => {
            tracing::warn!(op, error = %other, "mcp handler error");
            DispatchOutcome::Error {
                code: -32000,
                message: other.to_string(),
                data: None,
            }
        }
        Err(payload) => {
            let msg = panic_msg(payload);
            tracing::error!(op, panic = %msg, "mcp handler panicked");
            DispatchOutcome::Error {
                code: -32603,
                message: "internal error: handler panicked".into(),
                data: None,
            }
        }
    }
}

/// Map a non-handler `McpError` to a JSON-RPC error outcome. Kept
/// as a free fn for re-use by future transports that surface their
/// own handler errors outside the `Dispatcher` (e.g. HTTP body
/// parsing in 76.1).
#[allow(dead_code)] // reserved for 76.1 HTTP transport surface
pub(super) fn map_handler_error(err: McpError) -> DispatchOutcome {
    match err {
        McpError::Protocol(message) => DispatchOutcome::Error {
            code: -32602,
            message,
            data: None,
        },
        other => DispatchOutcome::Error {
            code: -32000,
            message: other.to_string(),
            data: None,
        },
    }
}

/// Pull the auth token out of an `initialize` params object.
/// Accepts both `params.auth_token` and `params._meta.auth_token`
/// for backward-compat with Phase 12.6 wire format.
pub(super) fn extract_auth_token(params: &Value) -> Option<&str> {
    params
        .get("auth_token")
        .and_then(|v| v.as_str())
        .or_else(|| {
            params
                .get("_meta")
                .and_then(|m| m.get("auth_token"))
                .and_then(|v| v.as_str())
        })
}

/// Constant-time string equality. Returns `false` immediately on
/// length mismatch; for equal lengths runs a XOR-accumulator over
/// every byte without short-circuiting per-byte.
///
/// **Caveat:** the length check itself is data-dependent — an
/// attacker can probe operator-configured token length by varying
/// input length. Acceptable for Phase 76.2 because (a) the operator
/// chooses a fixed-length token and never rotates the length and
/// (b) Phase 76.3 will replace this with the `subtle` crate's
/// `ConstantTimeEq` over a fixed-size bearer token.
pub(crate) fn consteq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    if ab.len() != bb.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..ab.len() {
        diff |= ab[i] ^ bb[i];
    }
    diff == 0
}

fn panic_msg(payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consteq_equal_strings() {
        assert!(consteq("hello", "hello"));
    }

    #[test]
    fn consteq_unequal_same_length() {
        assert!(!consteq("hello", "hellp"));
    }

    #[test]
    fn consteq_different_length() {
        assert!(!consteq("hello", "hellos"));
        assert!(!consteq("hellos", "hello"));
    }

    #[test]
    fn consteq_empty_strings() {
        assert!(consteq("", ""));
        assert!(!consteq("", "x"));
        assert!(!consteq("x", ""));
    }

    #[test]
    fn extract_auth_token_top_level() {
        let v = serde_json::json!({"auth_token": "abc"});
        assert_eq!(extract_auth_token(&v), Some("abc"));
    }

    #[test]
    fn extract_auth_token_meta_fallback() {
        let v = serde_json::json!({"_meta": {"auth_token": "xyz"}});
        assert_eq!(extract_auth_token(&v), Some("xyz"));
    }

    #[test]
    fn extract_auth_token_missing() {
        let v = serde_json::json!({});
        assert_eq!(extract_auth_token(&v), None);
    }

    #[test]
    fn completion_extracts_enum_values() {
        let tools = vec![McpTool {
            name: "my_tool".into(),
            description: Some("desc".into()),
            input_schema: serde_json::json!({
                "properties": {
                    "op": { "enum": ["read", "write", "delete"] }
                }
            }),
            output_schema: None,
        }];
        let params = serde_json::json!({
            "ref": { "type": "ref/tool", "name": "my_tool" },
            "argument": { "name": "op" }
        });
        let values = extract_completion_values(&tools, &params);
        assert_eq!(values, Some(vec!["read".into(), "write".into(), "delete".into()]));
    }

    #[test]
    fn completion_returns_none_for_unknown_tool() {
        let tools: Vec<McpTool> = vec![];
        let params = serde_json::json!({
            "ref": { "type": "ref/tool", "name": "missing" },
            "argument": { "name": "x" }
        });
        assert_eq!(extract_completion_values(&tools, &params), None);
    }

    #[test]
    fn completion_returns_none_when_no_enum() {
        let tools = vec![McpTool {
            name: "t".into(),
            description: None,
            input_schema: serde_json::json!({
                "properties": {
                    "text": { "type": "string" }
                }
            }),
            output_schema: None,
        }];
        let params = serde_json::json!({
            "ref": { "type": "ref/tool", "name": "t" },
            "argument": { "name": "text" }
        });
        assert_eq!(extract_completion_values(&tools, &params), None);
    }

    #[test]
    fn completion_returns_none_for_missing_arg() {
        let tools = vec![McpTool {
            name: "t".into(),
            description: None,
            input_schema: serde_json::json!({
                "properties": {
                    "op": { "enum": ["a"] }
                }
            }),
            output_schema: None,
        }];
        let params = serde_json::json!({
            "ref": { "type": "ref/tool", "name": "t" },
            "argument": { "name": "missing_prop" }
        });
        assert_eq!(extract_completion_values(&tools, &params), None);
    }
}
