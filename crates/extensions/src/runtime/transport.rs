//! Extension transport abstraction.
//!
//! 11.3 (stdio) and 11.4 (NATS) share the JSON-RPC 2.0 wire protocol; only
//! the byte carrier differs. `ExtensionTransport` lets downstream code
//! (tool registry in 11.5, hooks in 11.6) speak to either transport without
//! branching on variants.

use async_trait::async_trait;

use super::{CallError, HandshakeInfo, RuntimeState, ToolDescriptor};

#[async_trait]
pub trait ExtensionTransport: Send + Sync {
    fn extension_id(&self) -> &str;
    fn handshake(&self) -> &HandshakeInfo;
    fn state(&self) -> RuntimeState;

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CallError>;

    async fn tools_list(&self) -> Result<Vec<ToolDescriptor>, CallError> {
        let v = self.call("tools/list", serde_json::json!({})).await?;
        if let Some(arr) = v.get("tools").cloned() {
            serde_json::from_value(arr).map_err(CallError::Decode)
        } else {
            serde_json::from_value(v).map_err(CallError::Decode)
        }
    }

    async fn tools_call(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        self.call(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    async fn shutdown(&self);

    /// Phase 11.3 follow-up — like `shutdown` but threads a reason into
    /// the `shutdown` notification params. Default delegates to
    /// `shutdown` and ignores the reason so third-party mocks keep
    /// working unchanged.
    async fn shutdown_with_reason(&self, _reason: &str) {
        self.shutdown().await;
    }
}
