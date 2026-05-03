//! Phase 82.10.n — production [`ChannelCredentialPersister`]
//! for the whatsapp channel.
//!
//! WhatsApp credentials follow a different lifecycle than the
//! other channels: pairing happens out-of-band via the
//! `nexo/admin/pairing/*` admin RPC family + QR scan. The
//! pairing flow already writes its own session state under
//! `<state_root>/pairing/<instance>/`. So this persister is
//! intentionally a NO-OP: it gives the dispatcher a registered
//! handler for `channel = "whatsapp"` (so the back-compat
//! opaque-only fallback doesn't kick in) but does not duplicate
//! the pairing flow's writes.
//!
//! Probe returns `not_probed` because session health is the
//! pairing subsystem's concern (it surfaces via
//! `nexo/notify/pairing_status_changed` already).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use nexo_core::agent::admin_rpc::dispatcher::AdminRpcError;
use nexo_core::agent::admin_rpc::domains::credentials::ChannelCredentialPersister;
use nexo_tool_meta::admin::credentials::{reason_code, CredentialValidationOutcome};

/// No-op persister for the whatsapp channel; pairing owns the
/// real lifecycle.
pub struct WhatsappPersister;

impl WhatsappPersister {
    /// Build the persister.
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl Default for WhatsappPersister {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl ChannelCredentialPersister for WhatsappPersister {
    fn channel(&self) -> &str {
        "whatsapp"
    }

    fn validate_shape(
        &self,
        _payload: &Value,
        _metadata: &HashMap<String, Value>,
    ) -> Result<(), AdminRpcError> {
        // Accept any shape — pairing flow does the real
        // validation when the operator scans the QR.
        Ok(())
    }

    async fn persist(
        &self,
        _instance: Option<&str>,
        _payload: &Value,
        _metadata: &HashMap<String, Value>,
    ) -> Result<(), AdminRpcError> {
        // Intentional no-op: pairing flow writes its own state.
        Ok(())
    }

    async fn revoke(&self, _instance: Option<&str>) -> Result<bool, AdminRpcError> {
        // No-op: pairing flow has its own revoke surface
        // (`nexo/admin/pairing/cancel`). Returning false signals
        // "nothing changed at the persister layer" so the
        // dispatcher uses the opaque-store delete result alone
        // to decide whether to fire the reload.
        Ok(false)
    }

    async fn probe(
        &self,
        _instance: Option<&str>,
        _payload: &Value,
        _metadata: &HashMap<String, Value>,
    ) -> CredentialValidationOutcome {
        CredentialValidationOutcome {
            probed: false,
            healthy: false,
            detail: Some(
                "whatsapp session health is reported via nexo/notify/pairing_status_changed"
                    .into(),
            ),
            reason_code: Some(reason_code::NOT_PROBED.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn channel_id_is_whatsapp() {
        assert_eq!(WhatsappPersister::new().channel(), "whatsapp");
    }

    #[test]
    fn validate_shape_accepts_anything() {
        let p = WhatsappPersister::new();
        p.validate_shape(&json!(null), &HashMap::new()).unwrap();
        p.validate_shape(
            &json!({ "anything": "goes" }),
            &HashMap::new(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn persist_is_noop() {
        let p = WhatsappPersister::new();
        p.persist(Some("personal"), &json!({}), &HashMap::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn revoke_returns_false() {
        let p = WhatsappPersister::new();
        let removed = p.revoke(Some("personal")).await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn probe_returns_not_probed_with_explanation() {
        let p = WhatsappPersister::new();
        let outcome = p.probe(None, &json!({}), &HashMap::new()).await;
        assert!(!outcome.probed);
        assert!(!outcome.healthy);
        assert_eq!(outcome.reason_code.as_deref(), Some(reason_code::NOT_PROBED));
        assert!(outcome.detail.is_some());
    }
}
