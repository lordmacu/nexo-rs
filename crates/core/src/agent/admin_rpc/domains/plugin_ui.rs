//! Phase 99.5 — `nexo/admin/plugin_ui/{list,describe,config_set}`
//! admin RPC domain (Mode A).
//!
//! Daemon-owned handlers that turn each installed plugin's
//! `[plugin.admin_ui]` manifest section (Phase 99.1) into:
//!
//! - **list** — the menu / submenu structure, after slot + trust
//!   gating (Phase 99.4) + server-side `visible_when` (Phase 99.2).
//! - **describe** — a live [`ScreenDescriptor`]: SYNTHESISED from
//!   the manifest + current `cfg.plugins.<id>` values + credential
//!   status for `describe = false` plugins, or FORWARDED to the
//!   plugin over the broker (`[plugin.admin]`) for `describe = true`.
//! - **config_set** — validate (reusing the plugin's
//!   `config_schema`), route secret fields to the credential store
//!   (Phase 93), persist the rest to `cfg.plugins.<id>`, and fire
//!   the config reload (Phase 97).
//!
//! All external collaborators are injected behind traits so the
//! domain is unit-testable with fakes; the concrete adapters
//! (registry / plugins-yaml / broker forwarder / trust resolver)
//! are wired in `main.rs` boot.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use nexo_plugin_manifest::admin::PluginAdminSection;
use nexo_plugin_manifest::{
    eval_visible_when, parse_visible_when, validate_config, Action, Field, FieldType, I18nLabel,
    OptionsSource, PluginAdminUiSection,
};
use nexo_tool_meta::admin::agent_events::{AgentEventKind, PluginUiChangeKind};
use nexo_tool_meta::admin::plugin_discovery::TrustTier;
use nexo_tool_meta::admin::plugin_ui::{
    ActionView, ConfigFieldError, ConfigSetRequest, ConfigSetResponse, ContributionView,
    DescribeRequest, FieldDescriptor, PluginUiEntry, PluginUiListResponse, ScreenDescriptor,
    ScreenStub, SecretStatus, SelectOptionView,
};

use crate::admin_ui::slot_registry::SlotRegistry;
use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult, ReloadSignal};
use crate::agent::agent_events::AgentEventEmitter;

use super::credentials::CredentialStore;

/// Boxed firehose emitter handle passed from the dispatcher.
type Emitter = Option<Arc<dyn AgentEventEmitter>>;

/// Per-plugin manifest data the domain needs. Decouples the domain
/// from `NexoPluginRegistry` for testing.
#[derive(Clone, Debug)]
pub struct PluginUiManifestView {
    /// Plugin id (`google`).
    pub id: String,
    /// Human-readable plugin name.
    pub name: String,
    /// The plugin's `[plugin.admin_ui]` section.
    pub admin_ui: PluginAdminUiSection,
    /// The plugin's `[plugin.admin]` section (broker forward
    /// target). Required when `admin_ui.describe = true` or for
    /// `options_source = rpc` / custom save.
    pub admin: Option<PluginAdminSection>,
    /// The plugin's JSON-Schema (`config.schema.json`), used to
    /// validate `config_set` payloads.
    pub config_schema: Option<Value>,
}

/// Read-only view over installed plugins that contribute admin UI.
pub trait PluginUiRegistry: Send + Sync {
    /// Every installed plugin carrying a `[plugin.admin_ui]` section.
    fn admin_ui_views(&self) -> Vec<PluginUiManifestView>;
    /// One plugin by id (`None` when absent or no admin_ui).
    fn admin_ui_view(&self, plugin_id: &str) -> Option<PluginUiManifestView>;
}

/// Read / write the opaque `cfg.plugins.<id>` config slice.
pub trait PluginConfigStore: Send + Sync {
    /// Current config for `plugin_id` (`None` = unconfigured).
    fn read(&self, plugin_id: &str) -> anyhow::Result<Option<Value>>;
    /// Overwrite the config slice + persist to disk.
    fn write(&self, plugin_id: &str, config: &Value) -> anyhow::Result<()>;
}

/// Forward an admin method to a plugin subprocess over the broker.
#[async_trait::async_trait]
pub trait PluginRpcForwarder: Send + Sync {
    /// Forward `method` (under the plugin's `[plugin.admin]`
    /// `method_prefix`) with `params`; return the plugin's result
    /// payload or an error message.
    async fn forward(
        &self,
        admin: &PluginAdminSection,
        method: &str,
        params: Value,
    ) -> Result<Value, String>;
}

/// Resolve an installed plugin's trust tier (Phase 98 provenance).
pub trait PluginTrustResolver: Send + Sync {
    /// Trust tier for `plugin_id`.
    fn trust_of(&self, plugin_id: &str) -> TrustTier;
}

/// The `plugin_ui` admin RPC domain.
#[derive(Clone)]
pub struct PluginUiDomain {
    registry: Arc<dyn PluginUiRegistry>,
    config: Arc<dyn PluginConfigStore>,
    creds: Arc<dyn CredentialStore>,
    forwarder: Arc<dyn PluginRpcForwarder>,
    trust: Arc<dyn PluginTrustResolver>,
    reload: Option<ReloadSignal>,
    allow_community_core: bool,
    /// Operator locale for label resolution (BCP-47). v1 defaults
    /// to `"en"`; richer per-request locale is a follow-up.
    locale: String,
}

impl PluginUiDomain {
    /// Construct the domain from its injected collaborators.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: Arc<dyn PluginUiRegistry>,
        config: Arc<dyn PluginConfigStore>,
        creds: Arc<dyn CredentialStore>,
        forwarder: Arc<dyn PluginRpcForwarder>,
        trust: Arc<dyn PluginTrustResolver>,
        reload: Option<ReloadSignal>,
        allow_community_core: bool,
        locale: impl Into<String>,
    ) -> Self {
        Self {
            registry,
            config,
            creds,
            forwarder,
            trust,
            reload,
            allow_community_core,
            locale: locale.into(),
        }
    }

    // ── list ─────────────────────────────────────────────────────

    /// `nexo/admin/plugin_ui/list` — aggregate every plugin's
    /// gated contributions + screen stubs.
    pub fn list(&self) -> AdminRpcResult {
        let reg = SlotRegistry::core();
        let mut entries = Vec::new();
        for view in self.registry.admin_ui_views() {
            let tier = self.trust.trust_of(&view.id);
            let cfg = self
                .config
                .read(&view.id)
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            let ctx = json!({
                "plugin": { "installed": true, "enabled": true, "healthy": true },
                "config": cfg,
                "user": {},
                "tenant": {},
            });

            let mut contributions = Vec::new();
            let mut hidden_count: u32 = 0;
            for c in &view.admin_ui.contributions {
                // Effective slot = own slot, or the nearest ancestor's
                // slot (submenu items inherit placement).
                let Some(slot) = effective_slot(c, &view.admin_ui) else {
                    hidden_count += 1;
                    continue;
                };
                if !visible_ok(c.visible_when.as_deref(), &ctx) {
                    hidden_count += 1;
                    continue;
                }
                if !reg
                    .gate(slot, &view.id, tier, self.allow_community_core)
                    .is_allowed()
                {
                    hidden_count += 1;
                    continue;
                }
                contributions.push(ContributionView {
                    id: c.id.clone(),
                    slot: c.slot.clone(),
                    parent: c.parent.clone(),
                    label: self.label(&c.label),
                    icon: c.icon.clone(),
                    order: c.order,
                    screen: c.screen.clone(),
                });
            }

            let screens = view
                .admin_ui
                .screens
                .iter()
                .map(|s| ScreenStub {
                    id: s.id.clone(),
                    title: self.label(&s.title),
                })
                .collect();

            entries.push(PluginUiEntry {
                id: view.id.clone(),
                name: view.name.clone(),
                trust_tier: tier,
                contributions,
                screens,
                hidden_count,
            });
        }

        let etag = etag_for(&entries);
        let response = PluginUiListResponse {
            plugins: entries,
            etag,
        };
        AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
    }

    // ── describe ─────────────────────────────────────────────────

    /// `nexo/admin/plugin_ui/describe` — live screen descriptor.
    pub async fn describe(&self, params: Value) -> AdminRpcResult {
        let req: DescribeRequest = match serde_json::from_value(params) {
            Ok(r) => r,
            Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
        };
        let Some(view) = self.registry.admin_ui_view(&req.plugin) else {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "unknown plugin `{}`",
                req.plugin
            )));
        };

        // Dynamic plugins serve their own descriptor.
        if view.admin_ui.describe {
            return self.forward_describe(&view, &req).await;
        }

        // Static plugins: synthesise from the manifest screen.
        let Some(screen) = view.admin_ui.screens.iter().find(|s| s.id == req.screen) else {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "unknown screen `{}` for plugin `{}`",
                req.screen, req.plugin
            )));
        };
        let cfg = self
            .config
            .read(&view.id)
            .ok()
            .flatten()
            .unwrap_or(Value::Null);

        let mut fields = Vec::new();
        for f in &screen.fields {
            fields.push(self.synth_field(&view, f, &cfg).await);
        }
        let actions = screen.actions.iter().map(|a| self.action_view(a)).collect();
        let refresh =
            screen
                .refresh
                .as_ref()
                .map(|r| nexo_tool_meta::admin::plugin_ui::RefreshView {
                    method: r.method.clone(),
                    interval_seconds: r.interval_seconds,
                });

        let descriptor = ScreenDescriptor {
            plugin: view.id.clone(),
            screen_id: screen.id.clone(),
            title: self.label(&screen.title),
            fields,
            actions,
            refresh,
        };
        AdminRpcResult::ok(serde_json::to_value(descriptor).unwrap_or(Value::Null))
    }

    async fn forward_describe(
        &self,
        view: &PluginUiManifestView,
        req: &DescribeRequest,
    ) -> AdminRpcResult {
        let Some(admin) = &view.admin else {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "plugin `{}` sets admin_ui.describe=true but declares no [plugin.admin]",
                view.id
            )));
        };
        let method = format!("{}admin_ui/describe", admin.method_prefix);
        match self
            .forwarder
            .forward(
                admin,
                &method,
                serde_json::to_value(req).unwrap_or(Value::Null),
            )
            .await
        {
            Ok(result) => AdminRpcResult::ok(result),
            Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
                "describe forward to `{}` failed: {e}",
                view.id
            ))),
        }
    }

    /// Build one [`FieldDescriptor`] from a manifest field +
    /// current config + (for secrets) credential status +
    /// (for `options_source=rpc`) a broker-resolved choice set.
    async fn synth_field(
        &self,
        view: &PluginUiManifestView,
        f: &Field,
        cfg: &Value,
    ) -> FieldDescriptor {
        let is_secret = matches!(f.field_type, FieldType::Secret);
        let (value, secret) = if is_secret {
            (None, Some(self.secret_status(&view.id, &f.key)))
        } else {
            (cfg.get(&f.key).cloned(), None)
        };

        // Options: inline static, or RPC-resolved via the plugin.
        let options = self.resolve_options(view, f).await;

        FieldDescriptor {
            key: f.key.clone(),
            field_type: enum_str(&f.field_type),
            label: self.label(&f.label),
            required: f.required,
            help: f.help.as_ref().map(|h| self.label(h)),
            placeholder: f.placeholder.clone(),
            visible_when: f.visible_when.clone(),
            options,
            value,
            secret,
        }
    }

    async fn resolve_options(
        &self,
        view: &PluginUiManifestView,
        f: &Field,
    ) -> Option<Vec<SelectOptionView>> {
        // Inline static options win when present.
        if let Some(opts) = &f.options {
            return Some(
                opts.iter()
                    .map(|o| SelectOptionView {
                        value: o.value.clone(),
                        label: self.label(&o.label),
                    })
                    .collect(),
            );
        }
        // RPC-sourced options: forward to the plugin.
        if let Some(OptionsSource::Rpc { method }) = &f.options_source {
            let admin = view.admin.as_ref()?;
            match self.forwarder.forward(admin, method, json!({})).await {
                Ok(result) => serde_json::from_value::<Vec<SelectOptionView>>(result).ok(),
                Err(_) => None,
            }
        } else {
            None
        }
    }

    // ── config_set ───────────────────────────────────────────────

    /// `nexo/admin/plugin_ui/config_set` — validate, route secrets,
    /// persist, reload.
    pub async fn config_set(&self, params: Value, emitter: Emitter) -> AdminRpcResult {
        let req: ConfigSetRequest = match serde_json::from_value(params) {
            Ok(r) => r,
            Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
        };
        let Some(view) = self.registry.admin_ui_view(&req.plugin) else {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "unknown plugin `{}`",
                req.plugin
            )));
        };
        let screen = view.admin_ui.screens.iter().find(|s| s.id == req.screen);

        // A custom `id = "save"` action OR a dynamic plugin owns its
        // own persistence — forward the whole payload.
        if let Some(save) = screen.and_then(|s| s.actions.iter().find(|a| a.id == "save")) {
            return self
                .forward_action(&view, &save.method, req, &emitter)
                .await;
        }
        if view.admin_ui.describe {
            if let Some(admin) = &view.admin {
                let method = format!("{}admin_ui/config_set", admin.method_prefix);
                return self.forward_action(&view, &method, req, &emitter).await;
            }
        }

        // Daemon-default save (static plugin).
        let Some(screen) = screen else {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "unknown screen `{}` for plugin `{}`",
                req.screen, req.plugin
            )));
        };
        let secret_keys: BTreeSet<&str> = screen
            .fields
            .iter()
            .filter(|f| matches!(f.field_type, FieldType::Secret))
            .map(|f| f.key.as_str())
            .collect();

        // Split secrets out of the YAML-bound values.
        let mut normal = Map::new();
        let mut secrets = Map::new();
        for (k, v) in &req.values {
            if secret_keys.contains(k.as_str()) {
                secrets.insert(k.clone(), v.clone());
            } else {
                normal.insert(k.clone(), v.clone());
            }
        }

        // Validate the non-secret block against the plugin schema.
        if let Some(schema) = &view.config_schema {
            let errors = validate_config(&Value::Object(normal.clone()), schema);
            if !errors.is_empty() {
                let response = ConfigSetResponse {
                    ok: false,
                    reload_version: None,
                    errors: errors
                        .into_iter()
                        .map(|e| ConfigFieldError {
                            pointer: e.pointer,
                            message: e.message,
                        })
                        .collect(),
                };
                return AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null));
            }
        }

        // Route secrets to the credential store (never to YAML).
        for (k, v) in &secrets {
            if let Err(e) = self.creds.write_credential(&view.id, Some(k), v) {
                return AdminRpcResult::err(AdminRpcError::Internal(format!(
                    "credential write `{k}` failed: {e}"
                )));
            }
        }

        // Merge non-secret values into the existing config + persist.
        let mut merged = self
            .config
            .read(&view.id)
            .ok()
            .flatten()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        for (k, v) in normal {
            merged.insert(k, v);
        }
        if let Err(e) = self.config.write(&view.id, &Value::Object(merged)) {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "config write failed: {e}"
            )));
        }

        // Hot-apply.
        if let Some(reload) = &self.reload {
            reload();
        }
        self.emit_config_changed(&view.id, &emitter).await;

        let response = ConfigSetResponse {
            ok: true,
            reload_version: None,
            errors: vec![],
        };
        AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
    }

    async fn forward_action(
        &self,
        view: &PluginUiManifestView,
        method: &str,
        req: ConfigSetRequest,
        emitter: &Emitter,
    ) -> AdminRpcResult {
        let Some(admin) = &view.admin else {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "plugin `{}` needs [plugin.admin] to forward `{method}`",
                view.id
            )));
        };
        match self
            .forwarder
            .forward(
                admin,
                method,
                serde_json::to_value(&req).unwrap_or(Value::Null),
            )
            .await
        {
            Ok(result) => {
                // Plugin owns persistence; still fire reload so the
                // daemon picks up any config the plugin wrote.
                if let Some(reload) = &self.reload {
                    reload();
                }
                self.emit_config_changed(&view.id, emitter).await;
                AdminRpcResult::ok(result)
            }
            Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
                "config_set forward to `{}` failed: {e}",
                view.id
            ))),
        }
    }

    /// Phase 99.6 — fire `PluginUiChanged{ConfigChanged}` so admin
    /// shells re-fetch the affected screen. Best-effort (the
    /// emitter drops on transport failure).
    async fn emit_config_changed(&self, plugin_id: &str, emitter: &Emitter) {
        if let Some(em) = emitter {
            em.emit(AgentEventKind::PluginUiChanged {
                event_kind: PluginUiChangeKind::ConfigChanged,
                plugin_id: plugin_id.to_string(),
                admin_ui_present: true,
                trust_tier: self.trust.trust_of(plugin_id),
                at_ms: now_ms(),
            })
            .await;
        }
    }

    // ── helpers ──────────────────────────────────────────────────

    fn label(&self, label: &I18nLabel) -> String {
        match label {
            I18nLabel::Plain(s) => s.clone(),
            I18nLabel::Localized(m) => m
                .get(&self.locale)
                .or_else(|| m.values().next())
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn action_view(&self, a: &Action) -> ActionView {
        ActionView {
            id: a.id.clone(),
            label: self.label(&a.label),
            method: a.method.clone(),
            confirm: a.confirm.as_ref().map(|c| self.label(c)),
            prompt_fields: a
                .prompt_fields
                .iter()
                .map(|f| FieldDescriptor {
                    key: f.key.clone(),
                    field_type: enum_str(&f.field_type),
                    label: self.label(&f.label),
                    required: f.required,
                    help: f.help.as_ref().map(|h| self.label(h)),
                    placeholder: f.placeholder.clone(),
                    visible_when: f.visible_when.clone(),
                    options: f.options.as_ref().map(|opts| {
                        opts.iter()
                            .map(|o| SelectOptionView {
                                value: o.value.clone(),
                                label: self.label(&o.label),
                            })
                            .collect()
                    }),
                    value: None,
                    secret: None,
                })
                .collect(),
            on_success: enum_str(&a.on_success),
        }
    }

    fn secret_status(&self, plugin_id: &str, key: &str) -> SecretStatus {
        let exists = self
            .creds
            .list_credentials()
            .map(|list| {
                list.iter()
                    .any(|(ch, inst)| ch == plugin_id && inst.as_deref() == Some(key))
            })
            .unwrap_or(false);
        if exists {
            SecretStatus::Set
        } else {
            SecretStatus::Unset
        }
    }
}

/// Resolve a contribution's effective slot by walking up `parent`
/// links to the nearest ancestor declaring a `slot`. Returns
/// `None` for a dangling chain (manifest validation rejects cycles
/// + missing parents, so this is a defensive guard).
fn effective_slot<'a>(
    c: &'a nexo_plugin_manifest::Contribution,
    section: &'a PluginAdminUiSection,
) -> Option<&'a str> {
    let mut cur = c;
    for _ in 0..8 {
        if let Some(slot) = &cur.slot {
            return Some(slot.as_str());
        }
        let parent_id = cur.parent.as_deref()?;
        cur = section.contributions.iter().find(|x| x.id == parent_id)?;
    }
    None
}

/// Evaluate an optional `visible_when` against `ctx`. Absent =
/// visible; parse error = hidden (defensive).
fn visible_ok(expr: Option<&str>, ctx: &Value) -> bool {
    match expr {
        None => true,
        Some(src) => match parse_visible_when(src) {
            Ok(ast) => eval_visible_when(&ast, ctx),
            Err(_) => false,
        },
    }
}

/// Serde name of a `rename_all = "snake_case"` enum (FieldType /
/// OnSuccess), e.g. `FieldType::Secret` → `"secret"`.
fn enum_str<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|val| val.as_str().map(String::from))
        .unwrap_or_default()
}

/// Stable-ish ETag over the aggregated entries (hash of the JSON).
fn etag_for(entries: &[PluginUiEntry]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(entries)
        .unwrap_or_default()
        .hash(&mut h);
    format!("{:x}", h.finish())
}

/// Current epoch milliseconds (0 on a pre-epoch clock).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_plugin_manifest::admin_ui::{
        Contribution, FieldType, OnSuccess, Screen, SelectOption,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // ── fakes ────────────────────────────────────────────────────

    struct FakeRegistry(Vec<PluginUiManifestView>);
    impl PluginUiRegistry for FakeRegistry {
        fn admin_ui_views(&self) -> Vec<PluginUiManifestView> {
            self.0.clone()
        }
        fn admin_ui_view(&self, id: &str) -> Option<PluginUiManifestView> {
            self.0.iter().find(|v| v.id == id).cloned()
        }
    }

    #[derive(Default)]
    struct FakeConfig {
        store: Mutex<std::collections::BTreeMap<String, Value>>,
    }
    impl PluginConfigStore for FakeConfig {
        fn read(&self, id: &str) -> anyhow::Result<Option<Value>> {
            Ok(self.store.lock().unwrap().get(id).cloned())
        }
        fn write(&self, id: &str, cfg: &Value) -> anyhow::Result<()> {
            self.store
                .lock()
                .unwrap()
                .insert(id.to_string(), cfg.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeCreds {
        store: Mutex<Vec<(String, Option<String>, Value)>>,
    }
    impl CredentialStore for FakeCreds {
        fn list_credentials(&self) -> anyhow::Result<Vec<(String, Option<String>)>> {
            Ok(self
                .store
                .lock()
                .unwrap()
                .iter()
                .map(|(c, i, _)| (c.clone(), i.clone()))
                .collect())
        }
        fn write_credential(
            &self,
            channel: &str,
            instance: Option<&str>,
            payload: &Value,
        ) -> anyhow::Result<()> {
            self.store.lock().unwrap().push((
                channel.to_string(),
                instance.map(String::from),
                payload.clone(),
            ));
            Ok(())
        }
        fn delete_credential(&self, _c: &str, _i: Option<&str>) -> anyhow::Result<bool> {
            Ok(false)
        }
    }

    #[derive(Default)]
    struct FakeForwarder {
        // method substring -> reply
        replies: Mutex<Vec<(String, Value)>>,
        calls: Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl PluginRpcForwarder for FakeForwarder {
        async fn forward(
            &self,
            _admin: &PluginAdminSection,
            method: &str,
            _params: Value,
        ) -> Result<Value, String> {
            self.calls.lock().unwrap().push(method.to_string());
            for (needle, reply) in self.replies.lock().unwrap().iter() {
                if method.contains(needle.as_str()) {
                    return Ok(reply.clone());
                }
            }
            Err(format!("no fake reply for {method}"))
        }
    }

    struct FakeTrust(TrustTier);
    impl PluginTrustResolver for FakeTrust {
        fn trust_of(&self, _id: &str) -> TrustTier {
            self.0
        }
    }

    fn admin_section() -> PluginAdminSection {
        PluginAdminSection {
            method_prefix: "nexo/admin/google/".into(),
            broker_topic_prefix: "plugin.google.admin".into(),
            timeout_seconds: None,
        }
    }

    fn smtp_view(describe: bool) -> PluginUiManifestView {
        let toml = if describe {
            r#"
            describe = true
            [[contributions]]
            id = "google"
            slot = "core.sidebar.integrations"
            label = "Google"
            screen = "smtp"
            [[screens]]
            id = "smtp"
            title = "SMTP"
            "#
        } else {
            r#"
            describe = false
            [[contributions]]
            id = "google"
            slot = "core.sidebar.integrations"
            label = "Google"
            screen = "smtp"
            [[contributions]]
            id = "smtp-sub"
            parent = "google"
            label = "SMTP"
            screen = "smtp"
            [[screens]]
            id = "smtp"
            title = "SMTP"
            [[screens.fields]]
            key = "host"
            type = "text"
            label = "Host"
            required = true
            [[screens.fields]]
            key = "password"
            type = "secret"
            label = "Password"
            [[screens.actions]]
            id = "test"
            label = "Test"
            method = "nexo/admin/google/smtp_test"
            "#
        };
        PluginUiManifestView {
            id: "google".into(),
            name: "Google".into(),
            admin_ui: toml::from_str(toml).expect("admin_ui toml"),
            admin: Some(admin_section()),
            config_schema: Some(json!({
                "type": "object",
                "properties": { "host": {"type": "string"}, "port": {"type": "integer"} }
            })),
        }
    }

    fn domain_with(
        views: Vec<PluginUiManifestView>,
        tier: TrustTier,
        forwarder: Arc<FakeForwarder>,
        config: Arc<FakeConfig>,
        creds: Arc<FakeCreds>,
        reload: Option<ReloadSignal>,
        allow_community_core: bool,
    ) -> PluginUiDomain {
        PluginUiDomain::new(
            Arc::new(FakeRegistry(views)),
            config,
            creds,
            forwarder,
            Arc::new(FakeTrust(tier)),
            reload,
            allow_community_core,
            "en",
        )
    }

    fn ok_value(r: AdminRpcResult) -> Value {
        assert!(r.error.is_none(), "expected ok, got {:?}", r.error);
        r.result.unwrap()
    }

    #[test]
    fn list_official_includes_core_slot_contribution() {
        let d = domain_with(
            vec![smtp_view(false)],
            TrustTier::Official,
            Arc::new(FakeForwarder::default()),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        let resp: PluginUiListResponse = serde_json::from_value(ok_value(d.list())).unwrap();
        assert_eq!(resp.plugins.len(), 1);
        let g = &resp.plugins[0];
        // top-level "google" + submenu "smtp-sub" both survive.
        assert_eq!(g.contributions.len(), 2);
        assert_eq!(g.hidden_count, 0);
        assert!(!resp.etag.is_empty());
    }

    #[test]
    fn list_unverified_drops_core_slot_and_counts_hidden() {
        let d = domain_with(
            vec![smtp_view(false)],
            TrustTier::Unverified,
            Arc::new(FakeForwarder::default()),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        let resp: PluginUiListResponse = serde_json::from_value(ok_value(d.list())).unwrap();
        let g = &resp.plugins[0];
        // integrations is CoreReserved → unverified denied; submenu
        // inherits the same slot → both dropped.
        assert_eq!(g.contributions.len(), 0);
        assert_eq!(g.hidden_count, 2);
    }

    #[test]
    fn list_community_core_slot_allowed_with_override() {
        let d = domain_with(
            vec![smtp_view(false)],
            TrustTier::CommunityIndexed,
            Arc::new(FakeForwarder::default()),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            true,
        );
        let resp: PluginUiListResponse = serde_json::from_value(ok_value(d.list())).unwrap();
        assert_eq!(resp.plugins[0].contributions.len(), 2);
    }

    #[tokio::test]
    async fn describe_synthesises_static_screen_with_current_values() {
        let config = Arc::new(FakeConfig::default());
        config
            .write("google", &json!({"host": "smtp.gmail.com"}))
            .unwrap();
        let creds = Arc::new(FakeCreds::default());
        creds
            .write_credential("google", Some("password"), &json!("secret"))
            .unwrap();
        let d = domain_with(
            vec![smtp_view(false)],
            TrustTier::Official,
            Arc::new(FakeForwarder::default()),
            config,
            creds,
            None,
            false,
        );
        let resp = d
            .describe(json!({"plugin": "google", "screen": "smtp"}))
            .await;
        let descriptor: ScreenDescriptor = serde_json::from_value(ok_value(resp)).unwrap();
        let host = descriptor.fields.iter().find(|f| f.key == "host").unwrap();
        assert_eq!(host.value, Some(json!("smtp.gmail.com")));
        let pw = descriptor
            .fields
            .iter()
            .find(|f| f.key == "password")
            .unwrap();
        assert_eq!(pw.value, None);
        assert_eq!(pw.secret, Some(SecretStatus::Set));
        assert_eq!(descriptor.actions.len(), 1);
    }

    #[tokio::test]
    async fn describe_forwards_for_dynamic_plugin() {
        let forwarder = Arc::new(FakeForwarder::default());
        forwarder.replies.lock().unwrap().push((
            "admin_ui/describe".into(),
            json!({"plugin": "google", "screen_id": "smtp", "title": "Live", "fields": []}),
        ));
        let d = domain_with(
            vec![smtp_view(true)],
            TrustTier::Official,
            forwarder.clone(),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        let resp = d
            .describe(json!({"plugin": "google", "screen": "smtp"}))
            .await;
        let descriptor: ScreenDescriptor = serde_json::from_value(ok_value(resp)).unwrap();
        assert_eq!(descriptor.title, "Live");
        assert!(forwarder.calls.lock().unwrap()[0].contains("admin_ui/describe"));
    }

    #[tokio::test]
    async fn describe_unknown_plugin_is_invalid_params() {
        let d = domain_with(
            vec![],
            TrustTier::Official,
            Arc::new(FakeForwarder::default()),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        let resp = d.describe(json!({"plugin": "ghost", "screen": "x"})).await;
        assert!(matches!(resp.error, Some(AdminRpcError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn config_set_validates_routes_secret_persists_reloads() {
        let config = Arc::new(FakeConfig::default());
        let creds = Arc::new(FakeCreds::default());
        let reloaded = Arc::new(AtomicUsize::new(0));
        let r = reloaded.clone();
        let reload: ReloadSignal = Arc::new(move || {
            r.fetch_add(1, Ordering::SeqCst);
        });
        let d = domain_with(
            vec![smtp_view(false)],
            TrustTier::Official,
            Arc::new(FakeForwarder::default()),
            config.clone(),
            creds.clone(),
            Some(reload),
            false,
        );
        let resp = d
            .config_set(
                json!({
                    "plugin": "google",
                    "screen": "smtp",
                    "values": { "host": "smtp.gmail.com", "password": "hunter2" }
                }),
                None,
            )
            .await;
        let out: ConfigSetResponse = serde_json::from_value(ok_value(resp)).unwrap();
        assert!(out.ok, "{:?}", out.errors);
        // host persisted to config; password routed to cred store.
        let stored = config.read("google").unwrap().unwrap();
        assert_eq!(stored.get("host"), Some(&json!("smtp.gmail.com")));
        assert!(stored.get("password").is_none());
        let creds_list = creds.list_credentials().unwrap();
        assert!(creds_list
            .iter()
            .any(|(c, i)| c == "google" && i.as_deref() == Some("password")));
        assert_eq!(reloaded.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn config_set_rejects_invalid_against_schema() {
        let d = domain_with(
            vec![smtp_view(false)],
            TrustTier::Official,
            Arc::new(FakeForwarder::default()),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        // port must be integer; send a string.
        let resp = d
            .config_set(
                json!({
                    "plugin": "google", "screen": "smtp",
                    "values": { "port": "not-a-number" }
                }),
                None,
            )
            .await;
        let out: ConfigSetResponse = serde_json::from_value(ok_value(resp)).unwrap();
        assert!(!out.ok);
        assert!(out.errors.iter().any(|e| e.pointer == "/port"));
    }

    #[tokio::test]
    async fn config_set_forwards_for_dynamic_plugin() {
        let forwarder = Arc::new(FakeForwarder::default());
        forwarder
            .replies
            .lock()
            .unwrap()
            .push(("admin_ui/config_set".into(), json!({"ok": true})));
        let d = domain_with(
            vec![smtp_view(true)],
            TrustTier::Official,
            forwarder.clone(),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        let resp = d
            .config_set(
                json!({"plugin": "google", "screen": "smtp", "values": {}}),
                None,
            )
            .await;
        ok_value(resp);
        assert!(forwarder.calls.lock().unwrap()[0].contains("admin_ui/config_set"));
    }

    #[tokio::test]
    async fn config_set_emits_plugin_ui_changed() {
        #[derive(Debug, Default)]
        struct RecordingEmitter {
            events: std::sync::Mutex<Vec<AgentEventKind>>,
        }
        #[async_trait::async_trait]
        impl AgentEventEmitter for RecordingEmitter {
            async fn emit(&self, event: AgentEventKind) {
                self.events.lock().unwrap().push(event);
            }
        }
        let emitter = Arc::new(RecordingEmitter::default());
        let d = domain_with(
            vec![smtp_view(false)],
            TrustTier::Official,
            Arc::new(FakeForwarder::default()),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        let resp = d
            .config_set(
                json!({"plugin": "google", "screen": "smtp", "values": {"host": "x"}}),
                Some(emitter.clone()),
            )
            .await;
        ok_value(resp);
        let events = emitter.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AgentEventKind::PluginUiChanged {
                event_kind: PluginUiChangeKind::ConfigChanged,
                plugin_id,
                trust_tier: TrustTier::Official,
                ..
            } if plugin_id == "google"
        ));
    }

    #[tokio::test]
    async fn rpc_options_resolved_via_forwarder() {
        let forwarder = Arc::new(FakeForwarder::default());
        forwarder.replies.lock().unwrap().push((
            "calendars".into(),
            json!([{"value": "primary", "label": "Primary"}]),
        ));
        // Build a view whose screen has an rpc-options select field.
        let mut view = smtp_view(false);
        view.admin_ui = toml::from_str(
            r#"
            describe = false
            [[contributions]]
            id = "google"
            slot = "core.sidebar.integrations"
            label = "Google"
            screen = "cal"
            [[screens]]
            id = "cal"
            title = "Calendar"
            [[screens.fields]]
            key = "calendar"
            type = "select"
            label = "Calendar"
            [screens.fields.options_source]
            kind = "rpc"
            method = "nexo/admin/google/calendars"
            "#,
        )
        .unwrap();
        let d = domain_with(
            vec![view],
            TrustTier::Official,
            forwarder,
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        let resp = d
            .describe(json!({"plugin": "google", "screen": "cal"}))
            .await;
        let descriptor: ScreenDescriptor = serde_json::from_value(ok_value(resp)).unwrap();
        let cal = descriptor
            .fields
            .iter()
            .find(|f| f.key == "calendar")
            .unwrap();
        let opts = cal.options.as_ref().unwrap();
        assert_eq!(opts[0].value, "primary");
    }

    #[test]
    fn list_localized_label_resolves_to_locale() {
        let mut view = smtp_view(false);
        view.admin_ui.contributions[0].label = I18nLabel::Localized(
            [
                ("en".to_string(), "Google".to_string()),
                ("es".to_string(), "Google ES".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        let d = domain_with(
            vec![view],
            TrustTier::Official,
            Arc::new(FakeForwarder::default()),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        let resp: PluginUiListResponse = serde_json::from_value(ok_value(d.list())).unwrap();
        assert_eq!(resp.plugins[0].contributions[0].label, "Google");
    }

    #[test]
    fn visible_when_drops_contribution_server_side() {
        let mut view = smtp_view(false);
        // contribution visible only when config.enabled_flag is true;
        // config is empty → hidden.
        view.admin_ui.contributions[0].visible_when = Some("config.enabled_flag".into());
        // drop the submenu so only the top-level is counted.
        view.admin_ui.contributions.truncate(1);
        let d = domain_with(
            vec![view],
            TrustTier::Official,
            Arc::new(FakeForwarder::default()),
            Arc::new(FakeConfig::default()),
            Arc::new(FakeCreds::default()),
            None,
            false,
        );
        let resp: PluginUiListResponse = serde_json::from_value(ok_value(d.list())).unwrap();
        assert_eq!(resp.plugins[0].contributions.len(), 0);
        assert_eq!(resp.plugins[0].hidden_count, 1);
    }

    #[test]
    fn enum_str_maps_field_type_and_on_success() {
        assert_eq!(enum_str(&FieldType::Secret), "secret");
        assert_eq!(enum_str(&FieldType::Multiselect), "multiselect");
        assert_eq!(enum_str(&OnSuccess::InlineJson), "inline_json");
    }

    #[test]
    fn effective_slot_walks_parent_chain() {
        let section: PluginAdminUiSection = toml::from_str(
            r#"
            describe = true
            [[contributions]]
            id = "root"
            slot = "core.sidebar.root"
            label = "Root"
            [[contributions]]
            id = "child"
            parent = "root"
            label = "Child"
            screen = "x"
            [[screens]]
            id = "x"
            title = "X"
            "#,
        )
        .unwrap();
        let child = section
            .contributions
            .iter()
            .find(|c| c.id == "child")
            .unwrap();
        assert_eq!(effective_slot(child, &section), Some("core.sidebar.root"));
    }

    // Silence unused-import warnings for fixtures only used in some
    // cfg paths.
    #[allow(dead_code)]
    fn _fixture_types(_: Contribution, _: Screen, _: SelectOption) {}
}
