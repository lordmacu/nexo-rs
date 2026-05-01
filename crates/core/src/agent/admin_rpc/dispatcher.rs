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

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use thiserror::Error;

use super::audit::{
    hash_params, now_epoch_ms, AdminAuditResult, AdminAuditRow, AdminAuditWriter,
    InMemoryAuditWriter,
};
use super::capabilities::CapabilitySet;
use super::domains::agents::YamlPatcher;
use super::domains::credentials::CredentialStore;

/// Reload signal callback — invoked by domain handlers after
/// successful yaml mutations to trigger Phase 18 hot-reload.
/// Production wiring passes a closure that calls
/// `ConfigReloadCoordinator::trigger_reload`.
pub type ReloadSignal = Arc<dyn Fn() + Send + Sync>;

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

/// Phase 82.10 admin RPC dispatcher.
///
/// Routes `nexo/admin/<domain>/<method>` requests to handlers,
/// consults [`CapabilitySet`] for the operator-granted capability
/// before each call, writes one [`AdminAuditRow`] per dispatch.
#[derive(Clone)]
pub struct AdminRpcDispatcher {
    capabilities: Arc<CapabilitySet>,
    audit: Arc<dyn AdminAuditWriter>,
    /// Phase 82.10.c — yaml mutation surface used by the
    /// `agents` domain handlers. `None` = domain unavailable
    /// (returns -32601 for `nexo/admin/agents/*`). Production
    /// wiring constructs the adapter from `nexo_setup::yaml_patch`.
    agents_yaml: Option<Arc<dyn YamlPatcher>>,
    /// Phase 82.10.c — Phase 18 reload trigger called after
    /// successful yaml mutations. `None` = no-op (for early-boot
    /// tests).
    reload_signal: Option<ReloadSignal>,
    /// Phase 82.10.d — credential filesystem store. `None`
    /// disables `nexo/admin/credentials/*`.
    credential_store: Option<Arc<dyn CredentialStore>>,
}

impl std::fmt::Debug for AdminRpcDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminRpcDispatcher")
            .field("audit", &self.audit)
            .field("agents_yaml", &self.agents_yaml.is_some())
            .field("reload_signal", &self.reload_signal.is_some())
            .finish()
    }
}

impl Default for AdminRpcDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl AdminRpcDispatcher {
    /// Build a dispatcher with empty capability grants and an
    /// in-memory audit writer.
    pub fn new() -> Self {
        Self {
            capabilities: CapabilitySet::empty(),
            audit: Arc::new(InMemoryAuditWriter::new()),
            agents_yaml: None,
            reload_signal: None,
            credential_store: None,
        }
    }

    /// Replace the capability set. Boot wiring calls this once
    /// after [`super::validate_capabilities_at_boot`] returns OK.
    pub fn with_capabilities(mut self, capabilities: Arc<CapabilitySet>) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Replace the audit writer. Tests inject in-memory; SQLite
    /// writer lands in 82.10.g.
    pub fn with_audit_writer(mut self, writer: Arc<dyn AdminAuditWriter>) -> Self {
        self.audit = writer;
        self
    }

    /// Phase 82.10.c — install the agents domain. Production
    /// passes a `YamlPatcher` adapter wrapping
    /// `nexo_setup::yaml_patch::*`.
    pub fn with_agents_domain(
        mut self,
        yaml: Arc<dyn YamlPatcher>,
        reload: ReloadSignal,
    ) -> Self {
        self.agents_yaml = Some(yaml);
        self.reload_signal = Some(reload);
        self
    }

    /// Phase 82.10.d — install the credentials domain. Reuses
    /// the agents-domain `YamlPatcher` + `ReloadSignal` (must be
    /// installed first via `with_agents_domain`).
    pub fn with_credentials_domain(mut self, store: Arc<dyn CredentialStore>) -> Self {
        self.credential_store = Some(store);
        self
    }

    /// Capability required for each method. Method routing also
    /// happens here — `None` = unknown method.
    fn required_capability(method: &str) -> Option<&'static str> {
        match method {
            "nexo/admin/echo" => Some("_echo"),
            "nexo/admin/agents/list"
            | "nexo/admin/agents/get"
            | "nexo/admin/agents/upsert"
            | "nexo/admin/agents/delete" => Some("agents_crud"),
            "nexo/admin/credentials/list"
            | "nexo/admin/credentials/register"
            | "nexo/admin/credentials/revoke" => Some("credentials_crud"),
            // Other domains registered in 82.10.e-f.
            _ => None,
        }
    }

    /// Dispatch one admin RPC request.
    pub async fn dispatch(
        &self,
        microapp_id: &str,
        method: &str,
        params: Value,
    ) -> AdminRpcResult {
        let started = Instant::now();
        let started_at_ms = now_epoch_ms();
        let args_hash = hash_params(&params);

        // 1. Method routing — capability lookup serves double
        //    duty: identifies the method, names the gate.
        let Some(capability) = Self::required_capability(method) else {
            let row = AdminAuditRow {
                microapp_id: microapp_id.to_string(),
                method: method.to_string(),
                capability: "(unknown_method)".into(),
                args_hash,
                started_at_ms,
                result: AdminAuditResult::Error,
                duration_ms: started.elapsed().as_millis() as u64,
            };
            self.audit.append(row).await;
            return AdminRpcResult::err(AdminRpcError::MethodNotFound(format!(
                "no admin handler registered for `{method}`"
            )));
        };

        // 2. Capability gate — fail-closed if not granted.
        if !self.capabilities.check(microapp_id, capability) {
            let row = AdminAuditRow {
                microapp_id: microapp_id.to_string(),
                method: method.to_string(),
                capability: capability.to_string(),
                args_hash,
                started_at_ms,
                result: AdminAuditResult::Denied,
                duration_ms: started.elapsed().as_millis() as u64,
            };
            self.audit.append(row).await;
            return AdminRpcResult::err(AdminRpcError::CapabilityNotGranted {
                capability: capability.to_string(),
                method: method.to_string(),
                microapp_id: microapp_id.to_string(),
            });
        }

        // 3. Handler dispatch.
        let result = self.call_handler(microapp_id, method, params).await;

        // 4. Audit row.
        let audit_result = match &result {
            AdminRpcResult { error: Some(_), .. } => AdminAuditResult::Error,
            _ => AdminAuditResult::Ok,
        };
        let row = AdminAuditRow {
            microapp_id: microapp_id.to_string(),
            method: method.to_string(),
            capability: capability.to_string(),
            args_hash,
            started_at_ms,
            result: audit_result,
            duration_ms: started.elapsed().as_millis() as u64,
        };
        self.audit.append(row).await;
        result
    }

    /// Method router.
    async fn call_handler(
        &self,
        microapp_id: &str,
        method: &str,
        params: Value,
    ) -> AdminRpcResult {
        match method {
            "nexo/admin/echo" => AdminRpcResult::ok(serde_json::json!({
                "echoed": params,
                "microapp_id": microapp_id,
            })),
            "nexo/admin/agents/list" => match &self.agents_yaml {
                Some(yaml) => super::domains::agents::list(yaml.as_ref(), params),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agents domain not configured".into(),
                )),
            },
            "nexo/admin/agents/get" => match &self.agents_yaml {
                Some(yaml) => super::domains::agents::get(yaml.as_ref(), params),
                None => AdminRpcResult::err(AdminRpcError::Internal(
                    "agents domain not configured".into(),
                )),
            },
            "nexo/admin/agents/upsert" => match (&self.agents_yaml, &self.reload_signal) {
                (Some(yaml), Some(reload)) => {
                    let trigger = reload.clone();
                    super::domains::agents::upsert(yaml.as_ref(), params, &move || trigger())
                }
                _ => AdminRpcResult::err(AdminRpcError::Internal(
                    "agents domain not configured".into(),
                )),
            },
            "nexo/admin/agents/delete" => match (&self.agents_yaml, &self.reload_signal) {
                (Some(yaml), Some(reload)) => {
                    let trigger = reload.clone();
                    super::domains::agents::delete(yaml.as_ref(), params, &move || trigger())
                }
                _ => AdminRpcResult::err(AdminRpcError::Internal(
                    "agents domain not configured".into(),
                )),
            },
            "nexo/admin/credentials/list" => {
                match (&self.credential_store, &self.agents_yaml) {
                    (Some(store), Some(yaml)) => {
                        super::domains::credentials::list(store.as_ref(), yaml.as_ref(), params)
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "credentials domain not configured".into(),
                    )),
                }
            }
            "nexo/admin/credentials/register" => {
                match (&self.credential_store, &self.agents_yaml, &self.reload_signal) {
                    (Some(store), Some(yaml), Some(reload)) => {
                        let trigger = reload.clone();
                        super::domains::credentials::register(
                            store.as_ref(),
                            yaml.as_ref(),
                            params,
                            &move || trigger(),
                        )
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "credentials domain not configured".into(),
                    )),
                }
            }
            "nexo/admin/credentials/revoke" => {
                match (&self.credential_store, &self.agents_yaml, &self.reload_signal) {
                    (Some(store), Some(yaml), Some(reload)) => {
                        let trigger = reload.clone();
                        super::domains::credentials::revoke(
                            store.as_ref(),
                            yaml.as_ref(),
                            params,
                            &move || trigger(),
                        )
                    }
                    _ => AdminRpcResult::err(AdminRpcError::Internal(
                        "credentials domain not configured".into(),
                    )),
                }
            }
            // unreachable — `required_capability` already filtered
            // unknown methods before we got here. Defensive.
            other => AdminRpcResult::err(AdminRpcError::MethodNotFound(format!(
                "no admin handler registered for `{other}`"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn dispatcher_granting(microapp_id: &str, caps: &[&str]) -> AdminRpcDispatcher {
        let mut grants = HashMap::new();
        grants.insert(
            microapp_id.to_string(),
            caps.iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
        );
        AdminRpcDispatcher::new().with_capabilities(CapabilitySet::from_grants(grants))
    }

    #[tokio::test]
    async fn dispatch_echo_returns_params_when_echo_capability_granted() {
        let d = dispatcher_granting("agent-creator", &["_echo"]);
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
    async fn dispatch_echo_denies_when_capability_not_granted() {
        let d = AdminRpcDispatcher::new();
        let result = d
            .dispatch("agent-creator", "nexo/admin/echo", Value::Null)
            .await;
        let err = result.error.expect("error");
        match err {
            AdminRpcError::CapabilityNotGranted {
                capability,
                method,
                microapp_id,
            } => {
                assert_eq!(capability, "_echo");
                assert_eq!(method, "nexo/admin/echo");
                assert_eq!(microapp_id, "agent-creator");
            }
            other => panic!("expected CapabilityNotGranted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_unknown_method_returns_method_not_found() {
        let d = AdminRpcDispatcher::new();
        let result = d
            .dispatch("agent-creator", "nexo/admin/totally_unknown", Value::Null)
            .await;
        let err = result.error.expect("error");
        assert!(matches!(err, AdminRpcError::MethodNotFound(_)));
        assert_eq!(err.code(), -32601);
    }

    #[tokio::test]
    async fn audit_writer_records_each_call_with_args_hash() {
        let writer = Arc::new(InMemoryAuditWriter::new());
        let d = dispatcher_granting("agent-creator", &["_echo"])
            .with_audit_writer(writer.clone());
        let _ = d
            .dispatch(
                "agent-creator",
                "nexo/admin/echo",
                serde_json::json!({ "x": 1 }),
            )
            .await;
        let row = writer.last().expect("row recorded");
        assert_eq!(row.microapp_id, "agent-creator");
        assert_eq!(row.method, "nexo/admin/echo");
        assert_eq!(row.capability, "_echo");
        assert_eq!(row.result, AdminAuditResult::Ok);
        assert_eq!(row.args_hash.len(), 64); // sha256 hex
    }

    #[tokio::test]
    async fn audit_writer_records_denial_with_capability_field() {
        let writer = Arc::new(InMemoryAuditWriter::new());
        // No capability granted — denial path.
        let d = AdminRpcDispatcher::new().with_audit_writer(writer.clone());
        let _ = d
            .dispatch("agent-creator", "nexo/admin/echo", Value::Null)
            .await;
        let row = writer.last().expect("row recorded");
        assert_eq!(row.result, AdminAuditResult::Denied);
        assert_eq!(row.capability, "_echo");
    }

    #[tokio::test]
    async fn audit_writer_records_unknown_method_as_error() {
        let writer = Arc::new(InMemoryAuditWriter::new());
        let d = dispatcher_granting("agent-creator", &["_echo"])
            .with_audit_writer(writer.clone());
        let _ = d
            .dispatch("agent-creator", "nexo/admin/nonexistent", Value::Null)
            .await;
        let row = writer.last().expect("row recorded");
        assert_eq!(row.result, AdminAuditResult::Error);
        assert_eq!(row.capability, "(unknown_method)");
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
