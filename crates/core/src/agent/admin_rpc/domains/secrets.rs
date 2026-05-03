//! Phase 82.10.k — `nexo/admin/secrets/write` handler.
//!
//! Validates the input shape (regex-bounded `name`, byte-bounded
//! `value`) then delegates to a [`SecretsStore`] implementation.
//! Production adapter (`nexo_setup::secrets_store::FsSecretsStore`)
//! writes to `<state_root>/secrets/<NAME>.txt` (mode 0600) AND
//! `std::env::set_var(name, value)` so existing
//! `std::env::var(name)` consumers (LLM clients, plugin auth)
//! pick up the new value without a daemon restart.
//!
//! Tests use [`MockSecretsStore`] which captures invocations
//! without touching disk + env.

use async_trait::async_trait;
use nexo_tool_meta::admin::secrets::{SecretsWriteInput, SecretsWriteResponse};
use serde_json::Value;

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Backend-agnostic interface for persisting secrets. Production
/// adapter (`nexo_setup::secrets_store::FsSecretsStore`) writes
/// to `<state_root>/secrets/<NAME>.txt` mode 0600 + sets the
/// process env var. Tests use a mock with captured invocations.
#[async_trait]
pub trait SecretsStore: Send + Sync {
    /// Write `value` keyed by `name`. Returns the persisted path
    /// + whether an existing env value was overwritten.
    async fn write(
        &self,
        name: &str,
        value: &str,
    ) -> Result<SecretsWriteResponse, AdminRpcError>;
}

const NAME_RE: &str = r"^[A-Z][A-Z0-9_]{1,63}$";
const MIN_VALUE_BYTES: usize = 1;
const MAX_VALUE_BYTES: usize = 8192;

/// Dispatcher entry point. Validates the input, then forwards to
/// the configured `SecretsStore` impl. Audit redaction of the
/// `value` field is handled by the audit writer (Phase 82.10.h),
/// not here — the cleartext flows through this handler so the
/// store impl can persist it, but never lands in the audit DB.
pub async fn write(
    store: &dyn SecretsStore,
    raw_params: Value,
) -> AdminRpcResult {
    match try_write(store, raw_params).await {
        Ok(v) => AdminRpcResult::ok(v),
        Err(e) => AdminRpcResult::err(e),
    }
}

async fn try_write(
    store: &dyn SecretsStore,
    raw_params: Value,
) -> Result<Value, AdminRpcError> {
    let input: SecretsWriteInput = serde_json::from_value(raw_params)
        .map_err(|e| AdminRpcError::InvalidParams(e.to_string()))?;
    validate_name(&input.name)?;
    validate_value(&input.value)?;
    let response = store.write(&input.name, &input.value).await?;
    serde_json::to_value(response).map_err(|e| AdminRpcError::Internal(e.to_string()))
}

fn validate_name(name: &str) -> Result<(), AdminRpcError> {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(NAME_RE).expect("static regex compiles"));
    if !re.is_match(name) {
        return Err(AdminRpcError::InvalidParams(format!(
            "name {name:?} must match {NAME_RE}"
        )));
    }
    Ok(())
}

fn validate_value(value: &str) -> Result<(), AdminRpcError> {
    let n = value.as_bytes().len();
    if n < MIN_VALUE_BYTES {
        return Err(AdminRpcError::InvalidParams(
            "value cannot be empty".into(),
        ));
    }
    if n > MAX_VALUE_BYTES {
        return Err(AdminRpcError::InvalidParams(format!(
            "value length {n} exceeds {MAX_VALUE_BYTES}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::sync::Mutex as TokioMutex;

    /// Test double — records invocations + returns a canned
    /// response. No filesystem, no env mutation.
    struct MockSecretsStore {
        calls: TokioMutex<Vec<(String, String)>>,
    }

    impl MockSecretsStore {
        fn new() -> Self {
            Self {
                calls: TokioMutex::new(Vec::new()),
            }
        }

        async fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().await.clone()
        }
    }

    #[async_trait]
    impl SecretsStore for MockSecretsStore {
        async fn write(
            &self,
            name: &str,
            value: &str,
        ) -> Result<SecretsWriteResponse, AdminRpcError> {
            self.calls
                .lock()
                .await
                .push((name.to_string(), value.to_string()));
            Ok(SecretsWriteResponse {
                path: PathBuf::from(format!("/test/secrets/{name}.txt")),
                overwrote_env: false,
            })
        }
    }

    fn ok_input(name: &str, value: &str) -> Value {
        serde_json::json!({
            "name": name,
            "value": value,
        })
    }

    fn expect_invalid_params(result: AdminRpcResult, needle: &str) {
        let err = result.error.expect("expected error result");
        match err {
            AdminRpcError::InvalidParams(msg) => assert!(
                msg.contains(needle),
                "expected error to mention {needle:?}, got {msg:?}"
            ),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_validates_name_regex_lowercase() {
        let store = MockSecretsStore::new();
        let result = write(&store, ok_input("lowercase_key", "v")).await;
        expect_invalid_params(result, "must match");
        assert!(store.calls().await.is_empty(), "store must not be called");
    }

    #[tokio::test]
    async fn write_validates_name_regex_traversal_blocked() {
        let store = MockSecretsStore::new();
        let result = write(&store, ok_input("../etc/passwd", "v")).await;
        expect_invalid_params(result, "must match");
        assert!(store.calls().await.is_empty());
    }

    #[tokio::test]
    async fn write_rejects_empty_value() {
        let store = MockSecretsStore::new();
        let result = write(&store, ok_input("FOO_KEY", "")).await;
        expect_invalid_params(result, "empty");
        assert!(store.calls().await.is_empty());
    }

    #[tokio::test]
    async fn write_rejects_oversized_value() {
        let store = MockSecretsStore::new();
        let too_big = "x".repeat(MAX_VALUE_BYTES + 1);
        let result = write(&store, ok_input("FOO_KEY", &too_big)).await;
        expect_invalid_params(result, "exceeds");
        assert!(store.calls().await.is_empty());
    }

    #[tokio::test]
    async fn write_calls_store_with_validated_input() {
        let store = MockSecretsStore::new();
        let result = write(&store, ok_input("MINIMAX_API_KEY", "sk-test")).await;
        let value = result.result.expect("expected success");
        assert!(value.is_object());
        assert!(value["path"].is_string());
        let calls = store.calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "MINIMAX_API_KEY");
        assert_eq!(calls[0].1, "sk-test");
    }
}
