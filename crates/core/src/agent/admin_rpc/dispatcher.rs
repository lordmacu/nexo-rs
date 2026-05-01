//! Phase 82.10.a — admin RPC dispatcher core.
//!
//! Single entry point `AdminRpcDispatcher::dispatch(microapp_id,
//! method, params) -> AdminRpcResult` invoked by the microapp
//! transport adapter when a JSON-RPC frame with `app:` ID prefix
//! arrives. Returns the typed result/error pair; caller frames +
//! writes the response.
//!
//! Sub-phase scope:
//! - **82.10.a** (now): single mock `nexo/admin/echo` handler. No
//!   capability gate (always allow), no audit log. Validates
//!   wire-shape end-to-end before adding domain logic.
//! - **82.10.b**: capability gate + audit log writer. `echo` will
//!   require `agents_crud` (any granted capability suffices for
//!   echo testing).
//! - **82.10.c-f**: register actual domain handlers
//!   (agents/credentials/pairing/llm_providers/channels).

use serde_json::Value;
use thiserror::Error;

/// Typed admin RPC errors returned to the SDK side, matching the
/// JSON-RPC error code conventions documented in the spec.
#[non_exhaustive]
#[derive(Debug, Error, PartialEq)]
pub enum AdminRpcError {
    /// `-32601` — method name not registered or disabled.
    #[error("method_not_found: {0}")]
    MethodNotFound(String),
    /// `-32602` — caller-supplied params failed validation.
    #[error("invalid_params: {0}")]
    InvalidParams(String),
    /// `-32004` — operator did not grant `capability` to this
    /// microapp via `extensions.yaml.<id>.capabilities_grant`.
    /// Wired in 82.10.b.
    #[error("capability_not_granted: {capability} for method {method}")]
    CapabilityNotGranted {
        /// Required capability name.
        capability: String,
        /// Method that was rejected.
        method: String,
        /// Microapp that requested.
        microapp_id: String,
    },
    /// `-32603` — internal error.
    #[error("internal: {0}")]
    Internal(String),
}

impl AdminRpcError {
    /// Map to JSON-RPC error code for the wire frame.
    pub fn code(&self) -> i32 {
        match self {
            AdminRpcError::MethodNotFound(_) => -32601,
            AdminRpcError::InvalidParams(_) => -32602,
            AdminRpcError::CapabilityNotGranted { .. } => -32004,
            AdminRpcError::Internal(_) => -32603,
        }
    }

    /// Optional structured `data` field for the wire frame.
    pub fn data(&self) -> Option<Value> {
        match self {
            AdminRpcError::CapabilityNotGranted {
                capability,
                method,
                microapp_id,
            } => Some(serde_json::json!({
                "capability": capability,
                "microapp_id": microapp_id,
                "method": method,
            })),
            _ => None,
        }
    }
}

/// Dispatch result — the caller (microapp transport adapter)
/// frames it as `result` or `error`.
#[derive(Debug)]
pub struct AdminRpcResult {
    /// Successful payload. Mutually exclusive with `error`.
    pub result: Option<Value>,
    /// Error payload when dispatch failed.
    pub error: Option<AdminRpcError>,
}

impl AdminRpcResult {
    /// Build a success result.
    pub fn ok(value: Value) -> Self {
        Self {
            result: Some(value),
            error: None,
        }
    }

    /// Build an error result.
    pub fn err(e: AdminRpcError) -> Self {
        Self {
            result: None,
            error: Some(e),
        }
    }
}

/// Phase 82.10.a — minimal dispatcher with a single hard-coded
/// `nexo/admin/echo` route. Subsequent sub-phases extend this
/// with capability checks (82.10.b), audit log writes (82.10.b),
/// and domain handler registration (82.10.c-f).
#[derive(Debug, Default)]
pub struct AdminRpcDispatcher;

impl AdminRpcDispatcher {
    /// Build a dispatcher. Future sub-phases add fields
    /// (`Arc<CapabilitySet>`, `Arc<AdminAuditWriter>`, …).
    pub fn new() -> Self {
        Self
    }

    /// Dispatch one admin RPC request.
    ///
    /// `microapp_id` is supplied by the transport adapter (it knows
    /// which microapp's stdio it's reading from).
    /// `method` is the full JSON-RPC method string
    /// (`nexo/admin/<domain>/<method>`).
    /// `params` is the JSON-RPC `params` field — passed verbatim
    /// to the matching handler (or echoed for the mock route).
    pub async fn dispatch(
        &self,
        microapp_id: &str,
        method: &str,
        params: Value,
    ) -> AdminRpcResult {
        match method {
            "nexo/admin/echo" => {
                // Mock route — echoes back the params with the
                // microapp_id stamped, so the integration test
                // can verify the transport adapter passed the
                // right caller identity.
                AdminRpcResult::ok(serde_json::json!({
                    "echoed": params,
                    "microapp_id": microapp_id,
                }))
            }
            other => AdminRpcResult::err(AdminRpcError::MethodNotFound(format!(
                "no admin handler registered for `{other}`"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_echo_returns_params_unchanged_with_microapp_id() {
        let d = AdminRpcDispatcher::new();
        let result = d
            .dispatch(
                "agent-creator",
                "nexo/admin/echo",
                serde_json::json!({ "x": 1, "y": "hello" }),
            )
            .await;
        let value = result.result.expect("ok");
        assert_eq!(value["echoed"]["x"], 1);
        assert_eq!(value["echoed"]["y"], "hello");
        assert_eq!(value["microapp_id"], "agent-creator");
    }

    #[tokio::test]
    async fn dispatch_unknown_method_returns_method_not_found() {
        let d = AdminRpcDispatcher::new();
        let result = d
            .dispatch("agent-creator", "nexo/admin/agents/list", Value::Null)
            .await;
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::MethodNotFound(_)));
        assert_eq!(err.code(), -32601);
    }

    #[test]
    fn capability_not_granted_emits_structured_data() {
        let err = AdminRpcError::CapabilityNotGranted {
            capability: "agents_crud".into(),
            method: "nexo/admin/agents/upsert".into(),
            microapp_id: "agent-creator".into(),
        };
        assert_eq!(err.code(), -32004);
        let data = err.data().expect("structured data");
        assert_eq!(data["capability"], "agents_crud");
        assert_eq!(data["microapp_id"], "agent-creator");
        assert_eq!(data["method"], "nexo/admin/agents/upsert");
    }

    #[test]
    fn admin_rpc_error_code_table() {
        assert_eq!(AdminRpcError::MethodNotFound("x".into()).code(), -32601);
        assert_eq!(AdminRpcError::InvalidParams("x".into()).code(), -32602);
        assert_eq!(AdminRpcError::Internal("x".into()).code(), -32603);
    }
}
