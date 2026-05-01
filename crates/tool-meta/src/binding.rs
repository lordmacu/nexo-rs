//! [`BindingContext`] — the `(channel, account_id, agent_id,
//! session_id, binding_id)` tuple stamped on every tool call so a
//! downstream microapp knows which inbound binding the call
//! originated from.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifies which inbound binding a tool call originated from.
///
/// Stamped on every tool call dispatched to a Phase 11 stdio
/// extension or a Phase 12 MCP server (under `params._meta.nexo.binding`).
/// A microapp reads it to route work to the correct tenant /
/// channel / account.
///
/// `binding_id` is rendered as `<channel>:<account_id|"default">`
/// — a stable identifier that survives `agents.yaml` reloads
/// (does NOT depend on the binding vector index).
///
/// `mcp_channel_source` is populated when the inbound that
/// triggered the call arrived via a Phase 80.9 MCP channel
/// server (`"slack"`, `"telegram"`, etc.), allowing a tool to
/// distinguish "telegram-binding answered via MCP slack server"
/// from "telegram-binding answered via the native Telegram
/// plugin" while still seeing the same `(channel, account_id)`
/// tuple.
///
/// # Example
///
/// ```
/// use nexo_tool_meta::BindingContext;
///
/// let ctx = BindingContext::agent_only("ana");
/// assert_eq!(ctx.agent_id, "ana");
/// assert!(ctx.channel.is_none());
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingContext {
    /// Stable agent identifier (`agents.yaml.<id>`).
    pub agent_id: String,

    /// Active session UUID. `None` outside an LLM turn (heartbeat
    /// bootstrap, delegation receive, tests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,

    /// Channel name as declared in `InboundBinding.plugin`
    /// (`"whatsapp"`, `"telegram"`, `"email"`, `"web"`, …).
    /// `None` for contexts without a binding match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,

    /// Account / instance discriminator from
    /// `InboundBinding.instance`. `None` when the binding
    /// declared no instance (single-account default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,

    /// Stable binding identifier rendered as
    /// `<channel>:<account_id|"default">`. `None` for
    /// bindingless contexts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,

    /// Phase 80.9 — MCP channel server name when the inbound
    /// arrived via `notifications/nexo/channel`. `None` for
    /// native-channel inbounds. Examples: `"slack"`,
    /// `"telegram"`, `"imessage"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_channel_source: Option<String>,
}

impl BindingContext {
    /// Build the minimum context for a path that has no binding
    /// (delegation receive, heartbeat bootstrap, tests). Every
    /// optional field stays `None`.
    pub fn agent_only(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: None,
            channel: None,
            account_id: None,
            binding_id: None,
            mcp_channel_source: None,
        }
    }

    /// Builder helper: layer the MCP channel source on an
    /// already-built context. Lets the runtime distinguish a
    /// telegram-binding answered via MCP-slack from the same
    /// binding answered via the native Telegram plugin.
    pub fn with_mcp_channel_source(mut self, source: impl Into<String>) -> Self {
        self.mcp_channel_source = Some(source.into());
        self
    }

    /// Cheap accessor — returns the stable
    /// `<channel>:<account_id|"default">` identifier when
    /// available. `None` for bindingless contexts.
    pub fn binding_id(&self) -> Option<&str> {
        self.binding_id.as_deref()
    }
}

/// Render the canonical `<channel>:<account_id|"default">`
/// identifier. Pure-fn helper for callers that need to compute
/// the binding id outside of a fully-constructed
/// [`BindingContext`].
///
/// # Example
///
/// ```
/// use nexo_tool_meta::binding_id_render;
///
/// assert_eq!(
///     binding_id_render("whatsapp", Some("personal")),
///     "whatsapp:personal"
/// );
/// assert_eq!(binding_id_render("whatsapp", None), "whatsapp:default");
/// ```
pub fn binding_id_render(channel: &str, account_id: Option<&str>) -> String {
    let account = account_id.unwrap_or("default");
    format!("{channel}:{account}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_only_clears_optional_fields() {
        let ctx = BindingContext::agent_only("ana");
        assert_eq!(ctx.agent_id, "ana");
        assert!(ctx.session_id.is_none());
        assert!(ctx.channel.is_none());
        assert!(ctx.account_id.is_none());
        assert!(ctx.binding_id.is_none());
        assert!(ctx.mcp_channel_source.is_none());
    }

    #[test]
    fn with_mcp_channel_source_sets_field() {
        let ctx = BindingContext::agent_only("ana").with_mcp_channel_source("slack");
        assert_eq!(ctx.mcp_channel_source.as_deref(), Some("slack"));
        assert_eq!(ctx.agent_id, "ana");
    }

    #[test]
    fn binding_id_accessor_returns_str_or_none() {
        let mut ctx = BindingContext::agent_only("ana");
        assert!(ctx.binding_id().is_none());
        ctx.binding_id = Some("whatsapp:personal".into());
        assert_eq!(ctx.binding_id(), Some("whatsapp:personal"));
    }

    #[test]
    fn render_with_account_id() {
        assert_eq!(binding_id_render("whatsapp", Some("personal")), "whatsapp:personal");
        assert_eq!(binding_id_render("telegram", Some("kate_tg")), "telegram:kate_tg");
    }

    #[test]
    fn render_without_account_id_uses_default_sentinel() {
        assert_eq!(binding_id_render("whatsapp", None), "whatsapp:default");
    }

    #[test]
    fn serialise_skips_none_fields() {
        let ctx = BindingContext::agent_only("ana");
        let json = serde_json::to_value(&ctx).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("agent_id"));
        assert!(!obj.contains_key("session_id"));
        assert!(!obj.contains_key("channel"));
        assert!(!obj.contains_key("account_id"));
        assert!(!obj.contains_key("binding_id"));
        assert!(!obj.contains_key("mcp_channel_source"));
    }

    #[test]
    fn round_trip_through_serde() {
        let original = BindingContext::agent_only("carlos").with_mcp_channel_source("slack");
        let json = serde_json::to_string(&original).unwrap();
        let back: BindingContext = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn clone_eq_holds_for_full_payload() {
        let mut a = BindingContext::agent_only("ana");
        a.session_id = Some(Uuid::nil());
        a.channel = Some("whatsapp".into());
        a.account_id = Some("personal".into());
        a.binding_id = Some("whatsapp:personal".into());
        a.mcp_channel_source = Some("slack".into());
        let b = a.clone();
        assert_eq!(a, b);
    }
}
