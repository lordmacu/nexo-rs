//! `nexo/admin/plugins/*` handlers.
//!
//! Operator-facing snapshot of plugin discovery + spawn status.
//! Reads through a [`PluginDoctorReader`] trait so the production
//! impl can lift the same `wire_plugin_registry` + `doctor_render`
//! pipeline that powers the `agent doctor plugins` CLI without
//! the dispatcher pulling in `nexo-config` directly.
//!
//! Wire types: [`nexo_tool_meta::admin::plugin_doctor`].

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::plugin_doctor::PluginsDoctorResponse;

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Storage abstraction for the doctor snapshot. Production
/// concrete impl in `nexo-setup::admin_adapters` walks the
/// plugin discovery + capability aggregation pipeline + renders
/// the JSON shape `agent doctor plugins --json` already emits.
/// Tests inject a stub returning a canned `Value`.
#[async_trait]
pub trait PluginDoctorReader: Send + Sync + std::fmt::Debug {
    /// Build a fresh doctor report. Production impl is
    /// expensive (~hundreds of ms — filesystem walk + manifest
    /// parses + capability aggregation) so the dispatcher caches
    /// results when the operator hits the page repeatedly.
    /// v1 has no cache; every call re-runs.
    async fn report(&self) -> anyhow::Result<Value>;
}

/// `nexo/admin/plugins/doctor` — fresh discovery snapshot.
pub async fn doctor(reader: &dyn PluginDoctorReader) -> AdminRpcResult {
    match reader.report().await {
        Ok(report) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let resp = PluginsDoctorResponse {
                report,
                generated_at_ms: now,
            };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "plugin_doctor.report: {e}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct StubReader(Value);

    #[async_trait]
    impl PluginDoctorReader for StubReader {
        async fn report(&self) -> anyhow::Result<Value> {
            Ok(self.0.clone())
        }
    }

    #[derive(Debug)]
    struct ErrReader;

    #[async_trait]
    impl PluginDoctorReader for ErrReader {
        async fn report(&self) -> anyhow::Result<Value> {
            anyhow::bail!("simulated discovery failure")
        }
    }

    #[tokio::test]
    async fn doctor_wraps_report_with_timestamp() {
        let inner = serde_json::json!({"loaded_ids": ["telegram", "whatsapp"]});
        let reader = StubReader(inner.clone());
        let res = doctor(&reader).await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["report"], inner);
        assert!(payload["generated_at_ms"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn doctor_surfaces_internal_error_on_reader_failure() {
        let res = doctor(&ErrReader).await;
        assert!(res.error.is_some());
        assert!(res.result.is_none());
    }
}
