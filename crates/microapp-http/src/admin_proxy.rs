//! `POST /api/admin` — catch-all admin RPC proxy handler.
//!
//! Body: `{"method": "<nexo/admin/...>" , "params": {...}}`.
//! Forwards to the SDK's `AdminClient`. Successful responses
//! return `200 {"ok": true, "result": <admin-response>}`.
//! Errors map via [`crate::error::admin_error_to_response`] to
//! the right HTTP status + typed JSON body.
//!
//! ## Why a single endpoint
//!
//! A React UI consumes this as the single endpoint for every
//! admin RPC — no per-resource REST plumbing means new
//! `nexo/admin/*` methods (future framework phases) work
//! immediately without HTTP-side code.
//!
//! ## Operator identity stamping (Phase 82.10.m)
//!
//! Stamping moved from per-microapp middleware (path B) to the
//! SDK's `AdminClient` (path A). Microapps register a closure-
//! based source via `AdminClient::set_operator_token_hash` at
//! `on_admin_ready`; the SDK transparently stamps every
//! outbound stamped call. This handler is therefore a dumb
//! pass-through.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use nexo_microapp_sdk::admin::AdminClient;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::admin_error_to_response;

/// State shared across every `/api/admin` request. Holds the
/// admin client only — the SDK takes care of operator identity
/// stamping (see module-level docs).
pub struct AdminProxyState {
    /// Admin RPC client constructed via the SDK runtime.
    pub admin: Arc<AdminClient>,
}

/// Wire shape for `POST /api/admin` request bodies. `params`
/// defaults to `null` so callers can omit it for parameterless
/// methods.
#[derive(Debug, Deserialize)]
pub struct AdminRequest {
    /// Admin method name (`nexo/admin/<domain>/<action>`).
    pub method: String,
    /// Method-specific params. Forwarded as-is.
    #[serde(default)]
    pub params: Value,
}

/// Axum handler: forward the typed request to `AdminClient::call`,
/// map success / error to JSON.
pub async fn handler(
    State(s): State<Arc<AdminProxyState>>,
    Json(req): Json<AdminRequest>,
) -> Response {
    match s.admin.call::<Value, Value>(&req.method, req.params).await {
        Ok(result) => Json(json!({"ok": true, "result": result})).into_response(),
        Err(e) => admin_error_to_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as AxumRequest, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use http_body_util::BodyExt;
    use nexo_microapp_sdk::admin::MockAdminRpc;
    use tower::ServiceExt;

    fn router_with_mock(mock: &MockAdminRpc) -> Router {
        let admin = mock.client();
        let state = Arc::new(AdminProxyState {
            admin: Arc::new(admin),
        });
        Router::new()
            .route("/api/admin", post(handler))
            .with_state(state)
    }

    async fn body_of(res: Response) -> Value {
        let bytes = BodyExt::collect(res.into_body()).await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn post_admin(method: &str, params: Value) -> AxumRequest<Body> {
        let body = serde_json::to_vec(&json!({"method": method, "params": params})).unwrap();
        AxumRequest::builder()
            .method("POST")
            .uri("/api/admin")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn proxies_method_and_params_to_daemon() {
        let mock = MockAdminRpc::new();
        mock.on(
            "nexo/admin/agents/list",
            json!({"agents": [{"id": "ana"}]}),
        );
        let app = router_with_mock(&mock);
        let res = app
            .oneshot(post_admin("nexo/admin/agents/list", json!({})))
            .await
            .unwrap();
        let status = res.status();
        let v = body_of(res).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["ok"], true);
        assert_eq!(v["result"]["agents"][0]["id"], "ana");
        let calls = mock.requests_for("nexo/admin/agents/list");
        assert_eq!(calls.len(), 1);
    }

    #[tokio::test]
    async fn capability_not_granted_maps_to_403() {
        let mock = MockAdminRpc::new();
        mock.on_err(
            "nexo/admin/agents/upsert",
            nexo_microapp_sdk::admin::AdminError::CapabilityNotGranted {
                capability: "agents_crud".into(),
                method: "nexo/admin/agents/upsert".into(),
            },
        );
        let app = router_with_mock(&mock);
        let res = app
            .oneshot(post_admin("nexo/admin/agents/upsert", json!({"id": "ana"})))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let v = body_of(res).await;
        assert_eq!(v["error"]["code"], "capability_not_granted");
        assert_eq!(v["error"]["capability"], "agents_crud");
    }

    #[tokio::test]
    async fn invalid_params_maps_to_400() {
        let mock = MockAdminRpc::new();
        mock.on_err(
            "nexo/admin/agents/upsert",
            nexo_microapp_sdk::admin::AdminError::InvalidParams("missing id".into()),
        );
        let app = router_with_mock(&mock);
        let res = app
            .oneshot(post_admin("nexo/admin/agents/upsert", json!({})))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unregistered_method_maps_to_404() {
        let mock = MockAdminRpc::new();
        let app = router_with_mock(&mock);
        let res = app
            .oneshot(post_admin("nexo/admin/unknown/op", json!({})))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    /// Pass-through: when the SPA sends a stamped method
    /// (`processing/pause`), the proxy no longer modifies the
    /// params — stamping happens inside the SDK's `AdminClient`
    /// when a source is registered. The mock here has no
    /// source, so params reach it unchanged.
    #[tokio::test]
    async fn does_not_stamp_processing_pause_pass_through() {
        let mock = MockAdminRpc::new();
        mock.on("nexo/admin/processing/pause", json!({"ok": true}));
        let app = router_with_mock(&mock);
        let res = app
            .oneshot(post_admin(
                "nexo/admin/processing/pause",
                json!({
                    "scope": {
                        "kind": "conversation",
                        "agent_id": "ana",
                        "channel": "whatsapp",
                        "account_id": "",
                        "contact_id": "5491100"
                    },
                    "operator_token_hash": "client-supplied"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let calls = mock.requests_for("nexo/admin/processing/pause");
        // Mock client has no source → params untouched.
        assert_eq!(calls[0].params["operator_token_hash"], "client-supplied");
    }
}
