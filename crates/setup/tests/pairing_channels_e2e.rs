//! Phase 81.30 follow-up #6 — daemon-flavoured integration test
//! for `nexo/admin/pairing/channels`.
//!
//! Wires the real `PairingChannelsReaderImpl` against a synthetic
//! plugin handles cell + an in-memory credentials store, then
//! drives the dispatcher's RPC path end-to-end.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::agent::admin_rpc::capabilities::CapabilitySet;
use nexo_core::agent::admin_rpc::dispatcher::AdminRpcDispatcher;
use nexo_core::agent::admin_rpc::domains::credentials::CredentialStore;
use nexo_core::agent::plugin_host::{
    NexoPlugin, PluginInitContext, PluginInitError,
};
use nexo_plugin_manifest::PluginManifest;
use nexo_setup::admin_adapters::{
    shared_plugin_handles_cell, PairingChannelsReaderImpl, SharedPluginHandles,
};
use serde_json::{json, Value};

const TEST_MICROAPP: &str = "test_microapp";

fn capabilities_for_test() -> Arc<CapabilitySet> {
    let mut grants = std::collections::HashMap::new();
    let mut caps = std::collections::HashSet::new();
    caps.insert("pairing_initiate".to_string());
    grants.insert(TEST_MICROAPP.to_string(), caps);
    CapabilitySet::from_grants(grants)
}

#[derive(Debug)]
struct ManifestOnlyPlugin {
    manifest: PluginManifest,
}

#[async_trait]
impl NexoPlugin for ManifestOnlyPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    async fn init(
        &self,
        _ctx: &mut PluginInitContext<'_>,
    ) -> Result<(), PluginInitError> {
        unimplemented!("not exercised by e2e")
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn build_handles(plugins: Vec<(&str, &str)>) -> SharedPluginHandles {
    let cell = shared_plugin_handles_cell();
    let mut map = BTreeMap::new();
    for (id, toml_body) in plugins {
        let manifest: PluginManifest = toml::from_str(toml_body).expect("manifest parses");
        let plugin: Arc<dyn NexoPlugin> = Arc::new(ManifestOnlyPlugin { manifest });
        map.insert(id.to_string(), plugin);
    }
    *cell.write().await = Some(Arc::new(map));
    cell
}

#[derive(Debug, Default)]
struct StubCredentials(Vec<(String, Option<String>)>);

impl CredentialStore for StubCredentials {
    fn list_credentials(&self) -> anyhow::Result<Vec<(String, Option<String>)>> {
        Ok(self.0.clone())
    }
    fn write_credential(
        &self,
        _channel: &str,
        _instance: Option<&str>,
        _payload: &Value,
    ) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn delete_credential(
        &self,
        _channel: &str,
        _instance: Option<&str>,
    ) -> anyhow::Result<bool> {
        unimplemented!()
    }
}

const WHATSAPP_TOML: &str = r#"
manifest_version = 2
[plugin]
id = "whatsapp"
version = "0.1.0"
name = "WhatsApp"
description = "x"
min_nexo_version = ">=0.0.0"

[plugin.pairing]
kind = "qr"
label = "WhatsApp"

[plugin.pairing.instructions]
en = "Open WhatsApp and scan."
es = "Abre WhatsApp y escanea."
"#;

const TELEGRAM_TOML: &str = r#"
manifest_version = 2
[plugin]
id = "telegram"
version = "0.1.0"
name = "Telegram"
description = "x"
min_nexo_version = ">=0.0.0"

[plugin.pairing]
kind = "form"
label = "Telegram"

[[plugin.pairing.fields]]
name = "instance"
label = "Bot username"
required = true

[[plugin.pairing.fields]]
name = "token"
label = "Bot token"
sensitive = true
required = true
"#;

#[tokio::test]
async fn pairing_channels_returns_descriptor_for_each_loaded_plugin() {
    let handles = build_handles(vec![
        ("whatsapp", WHATSAPP_TOML),
        ("telegram", TELEGRAM_TOML),
    ])
    .await;
    let creds = Arc::new(StubCredentials(vec![(
        "whatsapp".to_string(),
        Some("549@s.whatsapp.net".to_string()),
    )]));
    let reader = PairingChannelsReaderImpl::new(handles, creds);
    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities_for_test())
        .with_pairing_channels(reader);

    let res = dispatcher
        .dispatch(
            TEST_MICROAPP,
            "nexo/admin/pairing/channels",
            json!({ "locale": "en" }),
        )
        .await;
    assert!(res.error.is_none(), "error: {:?}", res.error);
    let v = res.result.expect("ok");
    let channels = v.get("channels").unwrap().as_array().unwrap();
    assert_eq!(channels.len(), 2);

    // whatsapp comes before telegram (BTreeMap iteration order).
    let wa = &channels[1]; // telegram, whatsapp — alpha by plugin id
    let tg = &channels[0];
    assert_eq!(wa.get("channel").and_then(Value::as_str), Some("whatsapp"));
    assert_eq!(wa.get("kind").and_then(Value::as_str), Some("qr"));
    assert_eq!(
        wa.get("instructions").and_then(Value::as_str),
        Some("Open WhatsApp and scan.")
    );
    let linked = wa.get("linked_instances").unwrap().as_array().unwrap();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0], "549@s.whatsapp.net");

    assert_eq!(tg.get("channel").and_then(Value::as_str), Some("telegram"));
    assert_eq!(tg.get("kind").and_then(Value::as_str), Some("form"));
    let fields = tg.get("fields").unwrap().as_array().unwrap();
    assert_eq!(fields.len(), 2);
}

#[tokio::test]
async fn pairing_channels_locale_fallback_pt_br_to_en() {
    let handles = build_handles(vec![("whatsapp", WHATSAPP_TOML)]).await;
    let reader = PairingChannelsReaderImpl::new(handles, Arc::new(StubCredentials::default()));
    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities_for_test())
        .with_pairing_channels(reader);

    let res = dispatcher
        .dispatch(
            TEST_MICROAPP,
            "nexo/admin/pairing/channels",
            json!({ "locale": "pt-BR" }),
        )
        .await;
    let v = res.result.expect("ok");
    let ch = &v.get("channels").unwrap().as_array().unwrap()[0];
    // Falls back to `en` because no `pt-BR` / `pt` entry.
    assert_eq!(
        ch.get("instructions").and_then(Value::as_str),
        Some("Open WhatsApp and scan.")
    );
}

#[tokio::test]
async fn pairing_channels_skips_plugins_without_pairing_section() {
    let plain = r#"
manifest_version = 2
[plugin]
id = "browser"
version = "0.1.0"
name = "Browser"
description = "x"
min_nexo_version = ">=0.0.0"
"#;
    let handles = build_handles(vec![("browser", plain), ("whatsapp", WHATSAPP_TOML)]).await;
    let reader = PairingChannelsReaderImpl::new(handles, Arc::new(StubCredentials::default()));
    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities_for_test())
        .with_pairing_channels(reader);

    let res = dispatcher
        .dispatch(
            TEST_MICROAPP,
            "nexo/admin/pairing/channels",
            json!({}),
        )
        .await;
    let v = res.result.expect("ok");
    let channels = v.get("channels").unwrap().as_array().unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(
        channels[0].get("channel").and_then(Value::as_str),
        Some("whatsapp")
    );
}

#[tokio::test]
async fn pairing_channels_returns_domain_not_configured_when_reader_missing() {
    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities_for_test());
    let res = dispatcher
        .dispatch(
            TEST_MICROAPP,
            "nexo/admin/pairing/channels",
            json!({}),
        )
        .await;
    let err = res.error.expect("missing reader → error");
    let msg = format!("{err:?}");
    assert!(msg.contains("not configured"), "got: {msg}");
}
