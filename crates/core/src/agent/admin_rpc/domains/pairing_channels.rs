//! `nexo/admin/pairing/channels` handler.
//!
//! Plugin-driven pairing UI descriptor. Production impl in
//! `nexo-setup::admin_adapters` enumerates loaded plugin manifests
//! (via the shared plugin handles cell) and joins linked instances
//! from the credentials store. The dispatcher only knows the trait,
//! keeping the seam clean across the `nexo-core` ↔ `nexo-setup`
//! cycle break.
//!
//! Wire types: [`nexo_tool_meta::admin::pairing_channels`].

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use nexo_tool_meta::admin::pairing_channels::PairingChannelsResponse;

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Errors surfaced by the descriptor adapter. Today only one
/// variant — credentials store I/O failure — because manifest
/// enumeration is in-memory and infallible.
#[derive(Debug, Error)]
pub enum PairingChannelsError {
    /// `credentials/list` (or equivalent join source) failed.
    #[error("credentials store unavailable: {0}")]
    Credentials(String),
}

/// Read-only abstraction over the plugin manifest catalog + the
/// credentials join.
///
/// Production impl walks `wire_plugin_registry` (cached in a
/// `SharedPluginHandles` cell) for the manifest side and calls
/// `nexo/admin/credentials/list` for the join. Tests inject a
/// stub returning a canned [`PairingChannelsResponse`].
#[async_trait]
pub trait PairingChannelsReader: Send + Sync + std::fmt::Debug {
    /// Enumerate pair-able channels resolved against the requested
    /// BCP-47 `locale` tag. `locale` is `"en"` by default when the
    /// caller omitted it. Implementations resolve missing locales
    /// by falling back to `en` then the first instructions entry.
    async fn list(&self, locale: &str)
        -> Result<PairingChannelsResponse, PairingChannelsError>;
}

/// `nexo/admin/pairing/channels` — list pair-able channels.
///
/// Capability gate (`pairing_initiate`) is enforced upstream in
/// the dispatcher; this handler trusts that the caller is allowed.
pub async fn list_channels(
    reader: &dyn PairingChannelsReader,
    locale: &str,
) -> AdminRpcResult {
    match reader.list(locale).await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "pairing_channels.list: {e}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::pairing_channels::{
        PairingChannelInfo, PairingChannelKind, PairingChannelsResponse,
    };

    #[derive(Debug)]
    struct StubReader(PairingChannelsResponse);

    #[async_trait]
    impl PairingChannelsReader for StubReader {
        async fn list(
            &self,
            _locale: &str,
        ) -> Result<PairingChannelsResponse, PairingChannelsError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Debug)]
    struct ErrReader;

    #[async_trait]
    impl PairingChannelsReader for ErrReader {
        async fn list(
            &self,
            _locale: &str,
        ) -> Result<PairingChannelsResponse, PairingChannelsError> {
            Err(PairingChannelsError::Credentials("simulated".into()))
        }
    }

    #[tokio::test]
    async fn list_channels_returns_payload_on_ok() {
        let resp = PairingChannelsResponse {
            channels: vec![PairingChannelInfo {
                channel: "whatsapp".into(),
                kind: PairingChannelKind::Qr,
                label: "WhatsApp".into(),
                instructions: "scan".into(),
                fields: vec![],
                linked_instances: vec![],
                notify_method: None,
                instance_field: None,
            }],
        };
        let reader = StubReader(resp.clone());
        let res = list_channels(&reader, "en").await;
        let v = res.result.expect("ok");
        let back: PairingChannelsResponse = serde_json::from_value(v).unwrap();
        assert_eq!(back, resp);
    }

    #[tokio::test]
    async fn list_channels_surfaces_internal_error_on_reader_failure() {
        let res = list_channels(&ErrReader, "en").await;
        assert!(res.error.is_some());
        assert!(res.result.is_none());
    }
}
