//! Map `nexo_microapp_sdk::admin::AdminError` → HTTP status + JSON.
//!
//! Stable JSON shape `{"ok": false, "error": {"code": "...", ...}}`
//! so the React UI can branch on `code` regardless of HTTP status.
//! Lifted verbatim from `agent-creator-microapp::http::error`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use nexo_microapp_sdk::admin::AdminError;
use serde_json::{json, Value};

/// Map the SDK admin error to an HTTP response. Mapping table:
/// - `MethodNotFound` → 404
/// - `NotFound` → 404
/// - `InvalidParams` → 400
/// - `CapabilityNotGranted` → 403 + `capability` + `method` in body
/// - `Transport` → 503 (daemon unreachable)
/// - `Internal` (catch-all) → 500
pub fn admin_error_to_response(err: AdminError) -> Response {
    let (status, body) = match &err {
        AdminError::MethodNotFound(msg) => (
            StatusCode::NOT_FOUND,
            json!({"ok": false, "error": {"code": "method_not_found", "message": msg}}),
        ),
        AdminError::NotFound(msg) => (
            StatusCode::NOT_FOUND,
            json!({"ok": false, "error": {"code": "not_found", "message": msg}}),
        ),
        AdminError::InvalidParams(msg) => (
            StatusCode::BAD_REQUEST,
            json!({"ok": false, "error": {"code": "invalid_params", "message": msg}}),
        ),
        AdminError::CapabilityNotGranted { capability, method } => (
            StatusCode::FORBIDDEN,
            json!({
                "ok": false,
                "error": {
                    "code": "capability_not_granted",
                    "capability": capability,
                    "method": method,
                },
            }),
        ),
        AdminError::Transport(msg) => (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": {"code": "daemon_unreachable", "message": msg}}),
        ),
        // `#[non_exhaustive]` on AdminError — Internal + any future
        // variant the SDK gains lands here.
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"ok": false, "error": {"code": "internal", "message": err.to_string()}}),
        ),
    };
    (status, Json::<Value>(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use http_body_util::BodyExt;

    async fn body_of(res: Response) -> serde_json::Value {
        let bytes = BodyExt::collect(res.into_body()).await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn method_not_found_maps_to_404() {
        let res = admin_error_to_response(AdminError::MethodNotFound("nope".into()));
        let status = res.status();
        let v = body_of(res).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(v["error"]["code"], "method_not_found");
    }

    #[tokio::test]
    async fn not_found_maps_to_404() {
        let res = admin_error_to_response(AdminError::NotFound("x".into()));
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_params_maps_to_400() {
        let res = admin_error_to_response(AdminError::InvalidParams("bad".into()));
        let status = res.status();
        let v = body_of(res).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["code"], "invalid_params");
    }

    #[tokio::test]
    async fn capability_not_granted_maps_to_403_with_fields() {
        let res = admin_error_to_response(AdminError::CapabilityNotGranted {
            capability: "agents_crud".into(),
            method: "nexo/admin/agents/upsert".into(),
        });
        let status = res.status();
        let v = body_of(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(v["error"]["code"], "capability_not_granted");
        assert_eq!(v["error"]["capability"], "agents_crud");
        assert_eq!(v["error"]["method"], "nexo/admin/agents/upsert");
    }

    #[tokio::test]
    async fn transport_maps_to_503() {
        let res = admin_error_to_response(AdminError::Transport("eof".into()));
        let status = res.status();
        let v = body_of(res).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(v["error"]["code"], "daemon_unreachable");
    }

    #[tokio::test]
    async fn internal_maps_to_500() {
        let res = admin_error_to_response(AdminError::Internal("boom".into()));
        let status = res.status();
        let v = body_of(res).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(v["error"]["code"], "internal");
    }
}
