//! Plugin-driven pairing UI descriptor.
//!
//! Plugins that expose a channel which can be linked to an agent
//! declare HOW that link happens via `[plugin.pairing]` in their
//! `nexo-plugin.toml`. The admin reads this via the
//! `nexo/admin/pairing/channels` RPC and renders a modal driven
//! entirely by the descriptor — no per-channel hardcoded logic
//! in the admin frontend.
//!
//! Section is opt-in. Plugins without a channel (e.g. pure tool
//! plugins, MCP servers) omit the block entirely and never appear
//! in the admin's channel selector.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Pairing UI section in `nexo-plugin.toml`. Absent / unset =
/// plugin does not expose a pair-able channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PairingSection {
    /// Pairing flow kind. `None` = section present but plugin
    /// declares "no channel here". Treated identically to absent
    /// section by the admin (channel filtered out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PairingKind>,

    /// Human-visible label for the channel selector. When `None`,
    /// the admin falls back to `plugin.name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Operator-facing instructions per BCP-47 locale tag. The
    /// admin resolves the active locale, falls back to `en`, and
    /// finally to the first entry in iteration order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub instructions: BTreeMap<String, String>,

    /// Form-flow only (`kind = Form`). Fields the admin renders
    /// inside the modal. Empty for other kinds; if a plugin
    /// declares fields with a non-form kind they are ignored
    /// (logged at warn level when the admin reads the descriptor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<PairingFieldDescriptor>,

    /// Custom-flow only (`kind = Custom`). JSON-RPC notification
    /// method namespace the plugin pushes its progress on
    /// (`nexo/notify/<rpc_namespace>/status_changed`). When
    /// `None` and `kind = Custom`, the admin defaults to the
    /// plugin id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_namespace: Option<String>,

    /// Phase 81.30 follow-up #4 — name of the field (inside
    /// [`Self::fields`]) whose value the admin should treat as
    /// the credential's `instance` discriminator when calling
    /// `credentials/register`. `None` falls back to the literal
    /// `"instance"` for backwards compat with whatsapp + telegram
    /// (both ship a field called `instance`). Only meaningful
    /// when `kind = Form`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_field: Option<String>,
}

impl PairingSection {
    /// `true` when the manifest writer omitted every field.
    /// Equivalent to "no section present"; used by
    /// `skip_serializing_if` on the parent `PluginSection`.
    pub fn is_unset(&self) -> bool {
        self.kind.is_none()
            && self.label.is_none()
            && self.instructions.is_empty()
            && self.fields.is_empty()
            && self.rpc_namespace.is_none()
            && self.instance_field.is_none()
    }
}

/// Pairing flow kind. Closed enum to keep the admin's `switch`
/// statement exhaustive. New flow kinds require a manifest
/// schema bump; `Custom` covers escape-hatch use cases without
/// expanding this enum.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PairingKind {
    /// QR-code pairing (WhatsApp Web style). Admin renders the
    /// existing QR component which polls `pairing/start` +
    /// `pairing/status`.
    Qr,
    /// Form-based credential entry (Telegram bot token, …). Admin
    /// renders the `fields` list, posts the values to
    /// `credentials/register` on submit.
    Form,
    /// Informational only — channel is already configured
    /// out-of-band (YAML, env vars, …) and the admin just shows
    /// the instructions + a "Continue" button.
    Info,
    /// Plugin-defined flow. Admin opens the modal with
    /// `instructions`, subscribes to
    /// `nexo/notify/<rpc_namespace>/status_changed`, and closes
    /// on a terminal `state` ("linked" / "error" / "cancelled").
    Custom,
}

/// One field rendered inside the `Form`-flow modal.
///
/// Shape mirrors `crate::manifest::UiHint` (same vocabulary —
/// `label/help/sensitive/placeholder`) but adds `name` (the
/// stable key submitted back to `credentials/register`) and
/// `required` (admin blocks submit when missing).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PairingFieldDescriptor {
    /// Stable identifier — submitted to `credentials/register`
    /// verbatim. Snake_case by convention; the parser does not
    /// enforce a regex (consistency comes from review).
    pub name: String,

    /// Operator-visible label.
    pub label: String,

    /// Optional inline help text rendered under the input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,

    /// When `true`, admin renders `<input type="password">` and
    /// never logs the value. Default `false`.
    #[serde(default)]
    pub sensitive: bool,

    /// Optional placeholder shown when the input is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,

    /// When `true`, admin disables submit until the operator
    /// provides a non-empty value. Default `false`.
    #[serde(default)]
    pub required: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_section_is_unset() {
        let s = PairingSection::default();
        assert!(s.is_unset());
    }

    #[test]
    fn qr_kind_round_trips() {
        let toml_src = r#"
kind = "qr"
label = "WhatsApp"

[instructions]
es = "Abrí WhatsApp y escaneá."
en = "Open WhatsApp and scan."
"#;
        let parsed: PairingSection = toml::from_str(toml_src).unwrap();
        assert_eq!(parsed.kind, Some(PairingKind::Qr));
        assert_eq!(parsed.label.as_deref(), Some("WhatsApp"));
        assert_eq!(parsed.instructions.len(), 2);
        assert!(parsed.fields.is_empty());
    }

    #[test]
    fn form_kind_with_fields_round_trips() {
        let toml_src = r#"
kind = "form"
label = "Telegram"

[[fields]]
name = "instance"
label = "Bot username"
placeholder = "mi_bot"
required = true

[[fields]]
name = "token"
label = "Bot token"
sensitive = true
required = true
"#;
        let parsed: PairingSection = toml::from_str(toml_src).unwrap();
        assert_eq!(parsed.kind, Some(PairingKind::Form));
        assert_eq!(parsed.fields.len(), 2);
        assert_eq!(parsed.fields[0].name, "instance");
        assert!(parsed.fields[0].required);
        assert!(!parsed.fields[0].sensitive);
        assert!(parsed.fields[1].sensitive);
    }

    #[test]
    fn info_kind_without_fields_round_trips() {
        let toml_src = r#"
kind = "info"
[instructions]
en = "Configure plugin via YAML and restart."
"#;
        let parsed: PairingSection = toml::from_str(toml_src).unwrap();
        assert_eq!(parsed.kind, Some(PairingKind::Info));
        assert!(parsed.fields.is_empty());
    }

    #[test]
    fn custom_kind_with_namespace_round_trips() {
        let toml_src = r#"
kind = "custom"
rpc_namespace = "myauth"
"#;
        let parsed: PairingSection = toml::from_str(toml_src).unwrap();
        assert_eq!(parsed.kind, Some(PairingKind::Custom));
        assert_eq!(parsed.rpc_namespace.as_deref(), Some("myauth"));
    }

    #[test]
    fn unknown_kind_errors_with_clear_message() {
        let toml_src = r#"kind = "bluetooth""#;
        let err = toml::from_str::<PairingSection>(toml_src).unwrap_err();
        // serde's enum error mentions "unknown variant" — keeps
        // the message stable enough to assert on.
        assert!(
            err.to_string().contains("unknown variant"),
            "expected 'unknown variant' in error, got: {err}"
        );
    }

    #[test]
    fn deny_unknown_fields_rejects_typos() {
        let toml_src = r#"
kind = "qr"
laybel = "WhatsApp"
"#;
        let err = toml::from_str::<PairingSection>(toml_src).unwrap_err();
        assert!(
            err.to_string().contains("unknown field"),
            "expected 'unknown field' in error, got: {err}"
        );
    }

    #[test]
    fn skip_serializing_if_unset_emits_empty_toml() {
        let s = PairingSection::default();
        let out = toml::to_string(&s).unwrap();
        assert!(out.trim().is_empty(), "expected empty TOML, got: {out:?}");
    }
}
