//! `nexo/admin/pairing/channels` wire types.
//!
//! Plugin-driven pairing UI descriptor. The daemon enumerates
//! loaded plugin manifests, filters those with a `[plugin.pairing]`
//! section, joins linked instances from `credentials/list`, and
//! returns the catalog so the admin can render its channel selector
//! + per-channel modal entirely from data.
//!
//! See `crates/plugin-manifest/src/pairing.rs::PairingSection` for
//! the manifest-side shape; the wire types are a denormalised
//! locale-resolved projection of it.

use serde::{Deserialize, Serialize};

/// Params for `nexo/admin/pairing/channels`.
///
/// Currently only carries an optional BCP-47 locale tag used to
/// resolve the operator-visible `instructions` string. Future
/// filters (tenant scope, plugin id allowlist) land here without
/// breaking the wire envelope.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct PairingChannelsRequest {
    /// BCP-47 locale tag (`"es"`, `"en"`, `"pt-BR"`). When `None`
    /// the handler defaults to `"en"`. Resolution rule:
    /// exact match → base language → `"en"` → first entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

/// Response for `nexo/admin/pairing/channels`.
///
/// Channels are returned ordered by plugin id ascending so the
/// admin UI stays stable across refreshes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct PairingChannelsResponse {
    /// All channels exposed by the loaded plugins, joined with
    /// credential state.
    pub channels: Vec<PairingChannelInfo>,
}

/// One pair-able channel as exposed to the admin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct PairingChannelInfo {
    /// Stable channel id — matches `PairingStartInput::channel`
    /// verbatim. Typically equals the plugin id.
    pub channel: String,

    /// Pairing flow kind. Drives which modal branch the admin
    /// renders.
    pub kind: PairingChannelKind,

    /// Operator-visible label (`[plugin.pairing] label`, falling
    /// back to `plugin.name`).
    pub label: String,

    /// Locale-resolved instruction text. Empty string when the
    /// plugin did not ship any instructions block — the admin
    /// renders nothing in that case.
    #[serde(default)]
    pub instructions: String,

    /// Form-flow only — field descriptors to render inside the
    /// modal. Empty for the other kinds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<PairingChannelField>,

    /// Instances already linked (matched against `credentials/list`).
    /// Empty = channel has no credentials registered yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_instances: Vec<String>,

    /// Custom-flow only — JSON-RPC notify method the admin should
    /// subscribe to (`nexo/notify/<rpc_namespace>/status_changed`).
    /// `None` for non-custom kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_method: Option<String>,

    /// Phase 81.30 follow-up #4 — name of the form field whose
    /// value the admin should treat as the credential `instance`
    /// when calling `credentials/register`. `None` ⇒ the admin
    /// falls back to the literal `"instance"`. Only populated
    /// when `kind = "form"` and the plugin declared it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_field: Option<String>,
}

/// Pairing flow kind — wire mirror of
/// `nexo_plugin_manifest::PairingKind`. Kept as a separate enum
/// so the wire crate doesn't depend on `nexo-plugin-manifest`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum PairingChannelKind {
    /// QR-code pairing (WhatsApp Web style).
    Qr,
    /// Form-based credential entry (Telegram bot token, …).
    Form,
    /// Informational only — channel configured out-of-band.
    Info,
    /// Plugin-defined flow with notify-method subscription.
    Custom,
}

/// One field inside a `Form`-flow modal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct PairingChannelField {
    /// Stable key (submitted to `credentials/register` verbatim).
    pub name: String,
    /// Operator-visible label.
    pub label: String,
    /// Inline help text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// `true` ⇒ render as `<input type="password">`.
    pub sensitive: bool,
    /// Optional placeholder text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Admin blocks submit until the operator types a non-empty
    /// value.
    pub required: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_response_round_trips() {
        let r = PairingChannelsResponse::default();
        let v = serde_json::to_value(&r).unwrap();
        let back: PairingChannelsResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn qr_channel_round_trips() {
        let r = PairingChannelsResponse {
            channels: vec![PairingChannelInfo {
                channel: "whatsapp".into(),
                kind: PairingChannelKind::Qr,
                label: "WhatsApp".into(),
                instructions: "Open WhatsApp → ...".into(),
                fields: vec![],
                linked_instances: vec!["549@s.whatsapp.net".into()],
                notify_method: None,
                instance_field: None,
            }],
        };
        let v = serde_json::to_value(&r).unwrap();
        let back: PairingChannelsResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn form_channel_round_trips() {
        let r = PairingChannelsResponse {
            channels: vec![PairingChannelInfo {
                channel: "telegram".into(),
                kind: PairingChannelKind::Form,
                label: "Telegram".into(),
                instructions: "Paste your bot token.".into(),
                fields: vec![
                    PairingChannelField {
                        name: "instance".into(),
                        label: "Bot username".into(),
                        help: None,
                        sensitive: false,
                        placeholder: Some("mi_bot".into()),
                        required: true,
                    },
                    PairingChannelField {
                        name: "token".into(),
                        label: "Bot token".into(),
                        help: None,
                        sensitive: true,
                        placeholder: None,
                        required: true,
                    },
                ],
                linked_instances: vec![],
                notify_method: None,
                instance_field: None,
            }],
        };
        let v = serde_json::to_value(&r).unwrap();
        let back: PairingChannelsResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn skip_serializing_keeps_payload_small() {
        let r = PairingChannelsResponse {
            channels: vec![PairingChannelInfo {
                channel: "whatsapp".into(),
                kind: PairingChannelKind::Qr,
                label: "WhatsApp".into(),
                instructions: String::new(),
                fields: vec![],
                linked_instances: vec![],
                notify_method: None,
                instance_field: None,
            }],
        };
        let v = serde_json::to_value(&r).unwrap();
        let obj = v["channels"][0].as_object().unwrap();
        // empty defaults stay out of the JSON
        assert!(!obj.contains_key("fields"));
        assert!(!obj.contains_key("linked_instances"));
        assert!(!obj.contains_key("notify_method"));
    }

    #[test]
    fn kind_serialises_snake_case() {
        assert_eq!(
            serde_json::to_value(PairingChannelKind::Qr).unwrap(),
            serde_json::json!("qr")
        );
        assert_eq!(
            serde_json::to_value(PairingChannelKind::Custom).unwrap(),
            serde_json::json!("custom")
        );
    }
}
