//! `nexo/admin/plugin_ui/{list,describe,config_set}` wire types.
//!
//! Phase 99.3 — additive wire layer for the plugin admin-UI
//! contribution model (Mode A). The daemon aggregates each
//! installed plugin's `[plugin.admin_ui]` manifest section
//! (Phase 99.1) into a [`PluginUiListResponse`] (menu / submenu
//! structure) and serves a live [`ScreenDescriptor`] per screen —
//! synthesised from the manifest for static plugins
//! (`describe = false`) or forwarded to the plugin for dynamic
//! ones (`describe = true`).
//!
//! The frontend (`nexo-rs-plugin-admin`) consumes a UNIFORM
//! `ScreenDescriptor` regardless of origin (Phase 99.7/99.8).
//!
//! Trust tier reuses [`crate::admin::plugin_discovery::TrustTier`]
//! (derived from install provenance, never self-declared). Field
//! validation errors mirror the manifest crate's `ConfigSchemaError`
//! as [`ConfigFieldError`] — `nexo-tool-meta` does not depend on
//! `nexo-plugin-manifest`, so the daemon maps between the two.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::admin::plugin_discovery::TrustTier;

// ── list ─────────────────────────────────────────────────────────

/// Reply for `nexo/admin/plugin_ui/list` — every installed plugin
/// that contributes admin UI, with its menu structure. `etag`
/// supports `If-None-Match` so the frontend skips re-rendering the
/// rail when nothing changed.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginUiListResponse {
    /// Installed plugins that contribute admin UI.
    pub plugins: Vec<PluginUiEntry>,
    /// Aggregate hash of the response body; opaque to the client.
    pub etag: String,
}

/// One plugin's admin-UI contribution set.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginUiEntry {
    /// Plugin id (`google`).
    pub id: String,
    /// Human-readable plugin name (`Google`).
    pub name: String,
    /// Provenance-derived trust tier; gates which slots the
    /// plugin's contributions are allowed into (Phase 99.4).
    pub trust_tier: TrustTier,
    /// Menu / submenu / command-palette entries that survived
    /// trust + `visible_when` gating.
    pub contributions: Vec<ContributionView>,
    /// Screen stubs (id + resolved title) the contributions open.
    pub screens: Vec<ScreenStub>,
    /// Count of contributions dropped by trust-tier gating. The UI
    /// shows a "N hidden by trust tier" banner when non-zero.
    #[serde(default)]
    pub hidden_count: u32,
}

/// A menu / sidebar / command-palette entry, labels resolved for
/// the operator's locale.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContributionView {
    /// Contribution id, unique within the plugin.
    pub id: String,
    /// Target slot for a top-level entry; `None` when nested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// Parent contribution id when this is a submenu item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Display label, resolved for the operator's locale.
    pub label: String,
    /// Optional `lucide-react` icon name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Sort key (lower renders first).
    #[serde(default)]
    pub order: u32,
    /// Screen this entry opens. `None` for command-palette actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
}

/// Lightweight screen reference (full descriptor fetched lazily
/// via `describe`).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenStub {
    /// Screen id, referenced by `ContributionView.screen`.
    pub id: String,
    /// Screen heading, resolved for the operator's locale.
    pub title: String,
}

// ── describe ─────────────────────────────────────────────────────

/// Params for `nexo/admin/plugin_ui/describe`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DescribeRequest {
    /// Target plugin id.
    pub plugin: String,
    /// Target screen id.
    pub screen: String,
}

/// Live screen descriptor the generic renderer consumes. Built by
/// the daemon (synthesised or forwarded). Field values are current;
/// dynamic select options are already resolved.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenDescriptor {
    /// Owning plugin id.
    pub plugin: String,
    /// Screen id this descriptor renders.
    pub screen_id: String,
    /// Screen heading, locale-resolved.
    pub title: String,
    /// Form fields in render order.
    pub fields: Vec<FieldDescriptor>,
    /// Screen buttons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionView>,
    /// Optional read-only live widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<RefreshView>,
}

/// One resolved form field. For `secret` fields, `value` is ALWAYS
/// absent (write-only) and `secret` carries the set/unset status.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldDescriptor {
    /// Config key — maps to a `config_schema` property.
    pub key: String,
    /// Renderer kind — mirrors the manifest `FieldType` serde name
    /// (`text|number|secret|toggle|select|multiselect|list|link|textarea|json`).
    pub field_type: String,
    /// Field label, locale-resolved.
    pub label: String,
    /// Whether the field is required.
    #[serde(default)]
    pub required: bool,
    /// Optional help text, locale-resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// Optional placeholder shown in empty inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Optional `visible_when` expression evaluated client-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<String>,
    /// Resolved choice set (static or RPC-sourced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SelectOptionView>>,
    /// Current value. NEVER set for `secret` fields (write-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Set/unset status — present only for `secret` fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretStatus>,
}

/// Whether a secret credential is currently stored.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretStatus {
    /// A credential is stored for this field.
    Set,
    /// No credential stored yet.
    Unset,
}

/// One resolved select option.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectOptionView {
    /// Stored value.
    pub value: String,
    /// Display label, locale-resolved.
    pub label: String,
}

/// A screen button.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionView {
    /// Action id (`save` overrides the implicit save).
    pub id: String,
    /// Button label, locale-resolved.
    pub label: String,
    /// Admin RPC method dispatched on click (under `nexo/admin/`).
    pub method: String,
    /// Optional confirmation copy shown before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<String>,
    /// Optional inputs collected before dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_fields: Vec<FieldDescriptor>,
    /// Result-rendering mode — mirrors the manifest `OnSuccess`
    /// serde name (`toast|inline_json|table|redirect|refresh`).
    pub on_success: String,
}

/// A read-only live widget descriptor.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshView {
    /// Admin RPC returning the widget payload (under `nexo/admin/`).
    pub method: String,
    /// Optional auto-poll interval; `None` = manual refresh only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u64>,
}

// ── config_set ───────────────────────────────────────────────────

/// Params for `nexo/admin/plugin_ui/config_set` — the full form
/// payload (secret fields included; the daemon routes those to the
/// credential store and strips them from the YAML write).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigSetRequest {
    /// Target plugin id.
    pub plugin: String,
    /// Target screen id.
    pub screen: String,
    /// Submitted field values keyed by field `key`.
    pub values: BTreeMap<String, serde_json::Value>,
}

/// Reply for `config_set`. On validation failure `ok = false` and
/// `errors` lists each offending field; nothing is written and no
/// reload fires.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigSetResponse {
    /// `true` when the config validated, persisted, and reloaded.
    pub ok: bool,
    /// Config-reload version stamped after a successful hot-apply
    /// (Phase 97 `reload_signal`). `None` when validation failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_version: Option<u64>,
    /// Per-field validation failures (empty on success).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ConfigFieldError>,
}

/// One field-validation failure. Mirrors
/// `nexo_plugin_manifest::config_schema::ConfigSchemaError`
/// (the daemon maps between them since this crate has no manifest
/// dependency).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigFieldError {
    /// JSON Pointer to the offending field (`/port`).
    pub pointer: String,
    /// Operator-readable explanation.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roundtrip<T>(v: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let s = serde_json::to_string(v).expect("serialize");
        serde_json::from_str(&s).expect("deserialize")
    }

    #[test]
    fn list_response_roundtrip() {
        let r = PluginUiListResponse {
            plugins: vec![PluginUiEntry {
                id: "google".into(),
                name: "Google".into(),
                trust_tier: TrustTier::Official,
                contributions: vec![ContributionView {
                    id: "google".into(),
                    slot: Some("core.sidebar.integrations".into()),
                    parent: None,
                    label: "Google".into(),
                    icon: Some("mail".into()),
                    order: 1000,
                    screen: None,
                }],
                screens: vec![ScreenStub {
                    id: "smtp".into(),
                    title: "SMTP".into(),
                }],
                hidden_count: 0,
            }],
            etag: "abc123".into(),
        };
        assert_eq!(roundtrip(&r), r);
    }

    #[test]
    fn submenu_contribution_omits_slot_when_parented() {
        let c = ContributionView {
            id: "smtp".into(),
            slot: None,
            parent: Some("google".into()),
            label: "SMTP".into(),
            icon: None,
            order: 0,
            screen: Some("smtp".into()),
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("\"slot\""), "slot must be omitted: {s}");
        assert!(s.contains("\"parent\":\"google\""));
        assert_eq!(roundtrip(&c), c);
    }

    #[test]
    fn describe_request_roundtrip() {
        let r = DescribeRequest {
            plugin: "google".into(),
            screen: "smtp".into(),
        };
        assert_eq!(roundtrip(&r), r);
    }

    #[test]
    fn screen_descriptor_roundtrip() {
        let d = ScreenDescriptor {
            plugin: "google".into(),
            screen_id: "smtp".into(),
            title: "SMTP".into(),
            fields: vec![FieldDescriptor {
                key: "host".into(),
                field_type: "text".into(),
                label: "SMTP Host".into(),
                required: true,
                help: None,
                placeholder: Some("smtp.gmail.com".into()),
                visible_when: None,
                options: None,
                value: Some(json!("smtp.gmail.com")),
                secret: None,
            }],
            actions: vec![ActionView {
                id: "test".into(),
                label: "Test".into(),
                method: "nexo/admin/google/smtp_test".into(),
                confirm: None,
                prompt_fields: vec![],
                on_success: "toast".into(),
            }],
            refresh: Some(RefreshView {
                method: "nexo/admin/google/smtp_status".into(),
                interval_seconds: Some(30),
            }),
        };
        assert_eq!(roundtrip(&d), d);
    }

    #[test]
    fn secret_field_carries_status_never_value() {
        let f = FieldDescriptor {
            key: "password".into(),
            field_type: "secret".into(),
            label: "Password".into(),
            required: true,
            help: None,
            placeholder: None,
            visible_when: None,
            options: None,
            value: None,
            secret: Some(SecretStatus::Set),
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(
            !s.contains("\"value\""),
            "secret field must omit value: {s}"
        );
        assert!(s.contains("\"secret\":\"set\""));
        assert_eq!(roundtrip(&f), f);
    }

    #[test]
    fn secret_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&SecretStatus::Set).unwrap(),
            "\"set\""
        );
        assert_eq!(
            serde_json::to_string(&SecretStatus::Unset).unwrap(),
            "\"unset\""
        );
    }

    #[test]
    fn normal_field_carries_value_no_secret() {
        let f = FieldDescriptor {
            key: "port".into(),
            field_type: "number".into(),
            label: "Port".into(),
            required: false,
            help: None,
            placeholder: None,
            visible_when: Some("config.use_tls".into()),
            options: None,
            value: Some(json!(587)),
            secret: None,
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains("\"value\":587"));
        assert!(!s.contains("\"secret\""));
        assert_eq!(roundtrip(&f), f);
    }

    #[test]
    fn select_option_view_roundtrip() {
        let o = SelectOptionView {
            value: "bogota".into(),
            label: "Bogotá".into(),
        };
        assert_eq!(roundtrip(&o), o);
    }

    #[test]
    fn action_with_prompt_fields_roundtrip() {
        let a = ActionView {
            id: "send".into(),
            label: "Send test email".into(),
            method: "nexo/admin/google/smtp_send_test".into(),
            confirm: Some("Send a test email?".into()),
            prompt_fields: vec![FieldDescriptor {
                key: "to".into(),
                field_type: "text".into(),
                label: "Recipient".into(),
                required: true,
                help: None,
                placeholder: None,
                visible_when: None,
                options: None,
                value: None,
                secret: None,
            }],
            on_success: "table".into(),
        };
        assert_eq!(roundtrip(&a), a);
    }

    #[test]
    fn config_set_request_with_values_roundtrip() {
        let mut values = BTreeMap::new();
        values.insert("host".to_string(), json!("smtp.gmail.com"));
        values.insert("port".to_string(), json!(587));
        values.insert("use_tls".to_string(), json!(true));
        let r = ConfigSetRequest {
            plugin: "google".into(),
            screen: "smtp".into(),
            values,
        };
        assert_eq!(roundtrip(&r), r);
    }

    #[test]
    fn config_set_response_success_omits_errors() {
        let r = ConfigSetResponse {
            ok: true,
            reload_version: Some(42),
            errors: vec![],
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("\"errors\""));
        assert!(s.contains("\"reload_version\":42"));
        assert_eq!(roundtrip(&r), r);
    }

    #[test]
    fn config_set_response_failure_carries_errors() {
        let r = ConfigSetResponse {
            ok: false,
            reload_version: None,
            errors: vec![ConfigFieldError {
                pointer: "/port".into(),
                message: "expected type `integer`, got `string`".into(),
            }],
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("\"reload_version\""));
        assert!(s.contains("/port"));
        assert_eq!(roundtrip(&r), r);
    }

    #[test]
    fn trust_tier_reused_serializes_snake_case() {
        let e = PluginUiEntry {
            id: "x".into(),
            name: "X".into(),
            trust_tier: TrustTier::CommunityIndexed,
            contributions: vec![],
            screens: vec![],
            hidden_count: 2,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"trust_tier\":\"community_indexed\""));
        assert_eq!(roundtrip(&e), e);
    }

    #[test]
    fn list_response_default_is_empty() {
        let r = PluginUiListResponse::default();
        assert!(r.plugins.is_empty());
        assert_eq!(roundtrip(&r), r);
    }
}
