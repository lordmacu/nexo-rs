//! Phase 82.10.o — `nexo/admin/auth/rotate_token` handler.
//!
//! Validates the input shape (`new_token` length when supplied;
//! `reason` truncation deferred to the adapter) then delegates
//! to an [`AuthRotator`] implementation. Production adapter
//! (`nexo_setup::auth_rotator::FsAuthRotator`) persists the new
//! token to disk, emits `nexo/notify/token_rotated` to live
//! microapp listeners, and emits the matching audit firehose
//! event on the same broadcast channel.
//!
//! Tests use [`MockAuthRotator`] which captures invocations
//! without touching disk / spawning notifiers.

use async_trait::async_trait;
use nexo_tool_meta::admin::auth::{AuthRotateInput, AuthRotateResponse, MIN_TOKEN_LEN};
use nexo_tool_meta::http_server::TokenRotated;
use serde_json::Value;

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Push `nexo/notify/token_rotated` frames to live microapp
/// listeners. Implementations format the JSON-RPC notification
/// frame and write it to whatever sink the admin transport uses
/// (stdio in production, in-memory channel in tests).
///
/// Errors are logged-only; the daemon never blocks on
/// notification delivery — losing a frame is preferable to
/// stalling the rotation handler when a microapp reader is
/// laggy. Microapps that need every notification must hold the
/// listener registration AND poll a state-of-record endpoint
/// to reconcile.
pub trait TokenRotatedNotifier: Send + Sync {
    /// Push one token-rotated frame.
    fn notify_token_rotated(&self, payload: &TokenRotated);
}

/// Backend-agnostic interface for rotating the operator bearer.
/// Production adapter (`nexo_setup::auth_rotator::FsAuthRotator`)
/// persists to disk + emits the live notify + audit event. Tests
/// use a mock with captured invocations.
#[async_trait]
pub trait AuthRotator: Send + Sync {
    /// Rotate the operator bearer. Implementations:
    /// 1. Use `input.new_token` if `Some`, else generate a
    ///    cryptographically-random replacement.
    /// 2. Persist the new value (atomic, mode 0600).
    /// 3. Emit `nexo/notify/token_rotated { old_hash, new }`
    ///    to all connected microapp listeners.
    /// 4. Emit the audit firehose event
    ///    `AgentEventKind::SecurityEvent::TokenRotated`.
    async fn rotate(
        &self,
        input: AuthRotateInput,
    ) -> Result<AuthRotateResponse, AdminRpcError>;
}

/// Dispatcher entry point. Validates the supplied token (if
/// any) meets the [`MIN_TOKEN_LEN`] floor before forwarding.
/// Audit emit is the rotator's responsibility, not the
/// handler's — the audit row carries data only the rotator
/// knows (resolved hash, persistence timestamp).
pub async fn rotate_token(
    rotator: &dyn AuthRotator,
    raw_params: Value,
) -> AdminRpcResult {
    match try_rotate(rotator, raw_params).await {
        Ok(v) => AdminRpcResult::ok(v),
        Err(e) => AdminRpcResult::err(e),
    }
}

async fn try_rotate(
    rotator: &dyn AuthRotator,
    raw_params: Value,
) -> Result<Value, AdminRpcError> {
    let input: AuthRotateInput = serde_json::from_value(raw_params)
        .map_err(|e| AdminRpcError::InvalidParams(e.to_string()))?;
    if let Some(t) = input.new_token.as_deref() {
        if t.len() < MIN_TOKEN_LEN {
            return Err(AdminRpcError::InvalidParams(format!(
                "new_token must be >= {MIN_TOKEN_LEN} chars (got {})",
                t.len()
            )));
        }
    }
    let response = rotator.rotate(input).await?;
    serde_json::to_value(response).map_err(|e| AdminRpcError::Internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Mock that captures the input and returns a canned
    /// response. Tests inspect `invocations` to assert the
    /// handler forwarded the validated input.
    struct MockAuthRotator {
        invocations: Mutex<Vec<AuthRotateInput>>,
        response: AuthRotateResponse,
    }

    impl MockAuthRotator {
        fn new(response: AuthRotateResponse) -> Self {
            Self {
                invocations: Mutex::new(Vec::new()),
                response,
            }
        }
    }

    #[async_trait]
    impl AuthRotator for MockAuthRotator {
        async fn rotate(
            &self,
            input: AuthRotateInput,
        ) -> Result<AuthRotateResponse, AdminRpcError> {
            self.invocations.lock().await.push(input);
            Ok(self.response.clone())
        }
    }

    fn canned_response() -> AuthRotateResponse {
        AuthRotateResponse {
            ok: true,
            new_hash: "deadbeefcafebabe".into(),
            at_ms: 1_700_000_000_000,
        }
    }

    #[tokio::test]
    async fn rotate_with_valid_token_forwards_to_rotator_and_returns_response() {
        let rotator = Arc::new(MockAuthRotator::new(canned_response()));
        let result = rotate_token(
            rotator.as_ref(),
            json!({
                "new_token": "super-secret-bearer-32",
                "reason": "scheduled rotation",
            }),
        )
        .await;
        let v = result.result.expect("ok result");
        assert!(result.error.is_none());
        assert_eq!(v["ok"], true);
        assert_eq!(v["new_hash"], "deadbeefcafebabe");
        let calls = rotator.invocations.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].new_token.as_deref(), Some("super-secret-bearer-32"));
        assert_eq!(calls[0].reason.as_deref(), Some("scheduled rotation"));
    }

    #[tokio::test]
    async fn rotate_without_new_token_lets_rotator_generate() {
        let rotator = Arc::new(MockAuthRotator::new(canned_response()));
        let result = rotate_token(rotator.as_ref(), json!({})).await;
        assert!(result.error.is_none());
        let calls = rotator.invocations.lock().await;
        assert_eq!(calls.len(), 1);
        assert!(calls[0].new_token.is_none());
        assert!(calls[0].reason.is_none());
    }

    #[tokio::test]
    async fn rotate_rejects_short_token_without_reaching_rotator() {
        let rotator = Arc::new(MockAuthRotator::new(canned_response()));
        let result = rotate_token(
            rotator.as_ref(),
            json!({ "new_token": "short" }),
        )
        .await;
        let err = result.error.expect("err result");
        assert!(result.result.is_none());
        match err {
            AdminRpcError::InvalidParams(msg) => {
                assert!(msg.contains(">= 16"), "expected length floor in msg, got {msg}");
            }
            other => panic!("expected InvalidParams, got {other:?}"),
        }
        // Rotator must NOT have been called — validation rejects
        // before we reach the persistence layer.
        let calls = rotator.invocations.lock().await;
        assert!(calls.is_empty());
    }
}
