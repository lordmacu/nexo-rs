//! Phase 11.6 — bridge extension-registered hooks into the agent's
//! `HookRegistry`. The handler routes `on_hook(name, event)` to the owning
//! `StdioRuntime` via JSON-RPC method `hooks/<name>`.
use std::sync::Arc;
use std::time::Duration;
use async_trait::async_trait;
use serde_json::Value;
use agent_extensions::{HookResponse, StdioRuntime};
use super::hook_registry::HookHandler;
pub struct ExtensionHook {
    plugin_id: String,
    runtime: Arc<StdioRuntime>,
    /// Optional per-hook timeout. When `None` the runtime's default
    /// `call_timeout` applies. Lets operators tighten the bound for
    /// hot-path hooks (`before_message`) without shortening the
    /// tool-call budget.
    timeout: Option<Duration>,
}
impl ExtensionHook {
    pub fn new(plugin_id: impl Into<String>, runtime: Arc<StdioRuntime>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            runtime,
            timeout: None,
        }
    }
    /// Override the call timeout for this hook handler. Passing `None`
    /// restores the runtime's default `call_timeout`.
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
}
#[async_trait]
impl HookHandler for ExtensionHook {
    async fn on_hook(&self, name: &str, event: Value) -> anyhow::Result<HookResponse> {
        let method = format!("hooks/{name}");
        let raw = self
            .runtime
            .call_with_timeout(&method, event, self.timeout)
            .await
            .map_err(|e| anyhow::anyhow!("hook `{name}` on ext `{}`: {e}", self.plugin_id))?;
        // Tolerate `null` or empty-object responses (extensions that don't
        // bother returning anything) by falling back to default.
        if raw.is_null() {
            return Ok(HookResponse::default());
        }
        serde_json::from_value::<HookResponse>(raw).map_err(|e| {
            anyhow::anyhow!("invalid hook response from ext `{}`: {e}", self.plugin_id)
        })
    }
}
