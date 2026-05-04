//! Phase 81.3 — Tool namespace runtime enforcement.
//!
//! `ScopedToolRegistry` is a per-plugin proxy around `ToolRegistry`
//! that gates every registration against:
//!
//! 1. **Reserved-prefix denylist** ([`RESERVED_PREFIXES`]) — built-in
//!    namespaces like `agent_` / `system_` / `nexo_` / `mcp_` / `ext_`
//!    are off-limits to plugins (with the carve-out that
//!    `ext_<plugin_id>_*` is the canonical plugin-namespaced shape).
//! 2. **Plugin-scoped namespace** — every tool name MUST start with
//!    `<plugin_id>_` or `ext_<plugin_id>_`.
//! 3. **Manifest expose allowlist** — every registered tool MUST be
//!    declared in `manifest.plugin.tools.expose`. Operator sees the
//!    declared surface at install time; runtime registrations cannot
//!    silently grow it.
//! 4. **Collision rejection** — first-wins. A plugin cannot overwrite
//!    a built-in or another plugin's already-registered tool.
//!    Collision is rejected in BOTH `Warn` and `Strict` modes (silent
//!    overwrite is a footgun regardless of policy).
//!
//! `Warn` (default) records violations + logs + emits a broker event
//! but allows registration of namespace/expose violations to fall
//! through (back-compat for plugins not yet aware of the gate).
//! `Strict` (`NEXO_PLUGIN_NAMESPACE_STRICT=1`) returns `Err` on every
//! violation — the init loop translates accumulated violations into
//! `PluginInitError::ToolNamespace`, refusing to load the plugin.
//!
//! Built-in tool registrations from `main.rs` go through the raw
//! `ToolRegistry::register*` path unchanged — only plugin-side
//! callers see the scoped wrapper via `PluginInitContext`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nexo_broker::{AnyBroker, BrokerHandle};
use nexo_llm::ToolDef;
use serde::Serialize;

use crate::agent::extension_tool::{ExtensionTool, EXT_NAME_PREFIX};
use crate::agent::tool_registry::{ToolHandler, ToolMeta, ToolRegistry};

const BROKER_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Tool-name prefixes reserved for the host runtime. Plugins may
/// not register tools whose names begin with any of these strings,
/// regardless of the plugin's own id. The one exception is
/// `ext_<plugin_id>_…`, which is the canonical plugin-namespaced
/// shape — that path goes through dedicated extension-tool helpers
/// before reaching the registry, so the literal `ext_` prefix being
/// reserved here just stops a malicious plugin from forging
/// `ext_other_plugin_…` names.
pub const RESERVED_PREFIXES: &[&str] = &[
    "agent_",
    "system_",
    "nexo_",
    "mcp_",
    "ext_",
];

/// Whether the registry rejects out-of-namespace registrations or
/// merely logs them. Collisions are always rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceEnforcement {
    /// Log violations + emit broker event, but still register the
    /// tool when only the namespace/expose check failed. Collision
    /// always rejects.
    Warn,
    /// Reject every violation by returning `Err` from `register*`.
    Strict,
}

impl NamespaceEnforcement {
    /// Resolve mode from the `NEXO_PLUGIN_NAMESPACE_STRICT` env var.
    /// `"1"` / `"true"` → `Strict`; everything else → `Warn`.
    pub fn from_env() -> Self {
        match std::env::var("NEXO_PLUGIN_NAMESPACE_STRICT").as_deref() {
            Ok("1") | Ok("true") => Self::Strict,
            _ => Self::Warn,
        }
    }

    /// Stable lowercase string for log/JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Strict => "strict",
        }
    }
}

/// One namespace-policy violation recorded during plugin
/// registration. Surfaced via [`ScopedToolRegistry::drain_violations`]
/// and emitted as a broker event in real time.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NamespaceViolation {
    pub plugin_id: String,
    pub attempted_name: String,
    pub reason: NamespaceViolationReason,
}

/// Discriminator for the four kinds of namespace policy failures.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "detail")]
pub enum NamespaceViolationReason {
    /// Tool name starts with a reserved built-in prefix.
    ReservedPrefix(&'static str),
    /// Tool name doesn't start with `<plugin_id>_` or
    /// `ext_<plugin_id>_`.
    OutOfNamespace,
    /// Tool name not in the plugin's `manifest.tools.expose`.
    NotInExpose,
    /// Tool name already registered (host built-in or another plugin).
    Collision,
}

impl NamespaceViolationReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReservedPrefix(_) => "ReservedPrefix",
            Self::OutOfNamespace => "OutOfNamespace",
            Self::NotInExpose => "NotInExpose",
            Self::Collision => "Collision",
        }
    }

    pub fn reserved_prefix(&self) -> Option<&'static str> {
        match self {
            Self::ReservedPrefix(p) => Some(*p),
            _ => None,
        }
    }
}

/// Per-plugin proxy around `ToolRegistry`. Plugins receive an
/// `Arc<ScopedToolRegistry>` via `PluginInitContext`; the host calls
/// the inner `ToolRegistry` directly to register built-ins.
pub struct ScopedToolRegistry {
    plugin_id: String,
    /// Canonical (`ext_<id>_<tool>`) and bare (`<id>_<tool>`) shapes
    /// of every tool name the manifest's `tools.expose` declares.
    /// Empty when the plugin declares no tools — every register
    /// call then fails with `NotInExpose`.
    allowed_canonical: HashSet<String>,
    inner: Arc<ToolRegistry>,
    violations: Mutex<Vec<NamespaceViolation>>,
    mode: NamespaceEnforcement,
    broker: Option<AnyBroker>,
}

impl ScopedToolRegistry {
    pub fn new(
        plugin_id: String,
        manifest_expose: &[String],
        inner: Arc<ToolRegistry>,
        mode: NamespaceEnforcement,
        broker: Option<AnyBroker>,
    ) -> Self {
        let mut allowed_canonical = HashSet::with_capacity(manifest_expose.len() * 2);
        for raw in manifest_expose {
            // Accept both the bare `<id>_<name>` and the canonical
            // `ext_<id>_<name>` shapes — in-tree plugins sometimes
            // register without the `ext_` stamp.
            allowed_canonical.insert(raw.clone());
            allowed_canonical.insert(ExtensionTool::prefixed_name(&plugin_id, raw));
        }
        Self {
            plugin_id,
            allowed_canonical,
            inner,
            violations: Mutex::new(Vec::new()),
            mode,
            broker,
        }
    }

    pub fn mode(&self) -> NamespaceEnforcement {
        self.mode
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Register a tool. Validates namespace + expose + collision.
    /// Always rejects collisions; rejects namespace/expose violations
    /// in `Strict` mode.
    pub fn register(
        &self,
        def: ToolDef,
        handler: impl ToolHandler + 'static,
    ) -> Result<(), NamespaceViolation> {
        self.register_arc(def, Arc::new(handler))
    }

    /// Register an already-boxed handler. Same validation as
    /// `register`.
    pub fn register_arc(
        &self,
        def: ToolDef,
        handler: Arc<dyn ToolHandler>,
    ) -> Result<(), NamespaceViolation> {
        match self.validate_name(&def.name) {
            Ok(()) => self.commit_register(def, handler, None),
            Err(reason) => self.handle_violation(def, handler, reason, None),
        }
    }

    /// Register with metadata. Same validation as `register`.
    pub fn register_with_meta(
        &self,
        def: ToolDef,
        handler: impl ToolHandler + 'static,
        meta: ToolMeta,
    ) -> Result<(), NamespaceViolation> {
        let arc: Arc<dyn ToolHandler> = Arc::new(handler);
        match self.validate_name(&def.name) {
            Ok(()) => self.commit_register(def, arc, Some(meta)),
            Err(reason) => self.handle_violation(def, arc, reason, Some(meta)),
        }
    }

    /// Take ownership of recorded violations. Caller is the init
    /// loop after `NexoPlugin::init` returns.
    pub fn drain_violations(&self) -> Vec<NamespaceViolation> {
        let mut guard = self.violations.lock().expect("violations mutex poisoned");
        std::mem::take(&mut *guard)
    }

    // ── internals ────────────────────────────────────────────────

    fn validate_name(&self, name: &str) -> Result<(), NamespaceViolationReason> {
        // 1. Reserved prefix — `ext_<self_id>_…` is the legitimate
        // canonical shape, so it bypasses the literal `ext_` rule.
        let canonical_prefix = format!("{}{}_", EXT_NAME_PREFIX, self.plugin_id);
        for prefix in RESERVED_PREFIXES {
            if name.starts_with(prefix) {
                if *prefix == EXT_NAME_PREFIX && name.starts_with(&canonical_prefix) {
                    // Legitimate canonical plugin shape; continue.
                    break;
                }
                return Err(NamespaceViolationReason::ReservedPrefix(prefix));
            }
        }
        // 2. Out-of-namespace — must start with `<id>_` or `ext_<id>_`.
        let bare_prefix = format!("{}_", self.plugin_id);
        if !name.starts_with(&bare_prefix) && !name.starts_with(&canonical_prefix) {
            return Err(NamespaceViolationReason::OutOfNamespace);
        }
        // 3. Not in expose.
        if !self.allowed_canonical.contains(name) {
            return Err(NamespaceViolationReason::NotInExpose);
        }
        Ok(())
    }

    fn commit_register(
        &self,
        def: ToolDef,
        handler: Arc<dyn ToolHandler>,
        meta: Option<ToolMeta>,
    ) -> Result<(), NamespaceViolation> {
        let name = def.name.clone();
        let inserted = self.inner.register_if_absent_arc(def, handler);
        if !inserted {
            let v = NamespaceViolation {
                plugin_id: self.plugin_id.clone(),
                attempted_name: name,
                reason: NamespaceViolationReason::Collision,
            };
            self.record_and_emit(&v, true);
            return Err(v);
        }
        if let Some(m) = meta {
            self.inner.set_meta(&name, m);
        }
        Ok(())
    }

    fn handle_violation(
        &self,
        def: ToolDef,
        handler: Arc<dyn ToolHandler>,
        reason: NamespaceViolationReason,
        meta: Option<ToolMeta>,
    ) -> Result<(), NamespaceViolation> {
        let v = NamespaceViolation {
            plugin_id: self.plugin_id.clone(),
            attempted_name: def.name.clone(),
            reason,
        };
        match self.mode {
            NamespaceEnforcement::Strict => {
                self.record_and_emit(&v, true);
                Err(v)
            }
            NamespaceEnforcement::Warn => {
                // Warn mode: record + emit, but still attempt
                // registration (back-compat) UNLESS the slot is
                // already taken — collision check runs anyway.
                self.record_and_emit(&v, false);
                let inserted = self.inner.register_if_absent_arc(def, handler);
                if !inserted {
                    let collision = NamespaceViolation {
                        plugin_id: self.plugin_id.clone(),
                        attempted_name: v.attempted_name.clone(),
                        reason: NamespaceViolationReason::Collision,
                    };
                    self.record_and_emit(&collision, true);
                    return Err(collision);
                }
                if let Some(m) = meta {
                    self.inner.set_meta(&v.attempted_name, m);
                }
                Ok(())
            }
        }
    }

    fn record_and_emit(&self, v: &NamespaceViolation, rejected: bool) {
        tracing::warn!(
            plugin_id = %v.plugin_id,
            tool = %v.attempted_name,
            reason = %v.reason.as_str(),
            mode = %self.mode.as_str(),
            rejected,
            "tool namespace violation",
        );
        if let Ok(mut guard) = self.violations.lock() {
            guard.push(v.clone());
        }
        if let Some(broker) = self.broker.clone() {
            let plugin_id = v.plugin_id.clone();
            let attempted_name = v.attempted_name.clone();
            let reason = v.reason;
            let mode = self.mode.as_str();
            tokio::spawn(async move {
                emit_violation_event(broker, plugin_id, attempted_name, reason, mode, rejected)
                    .await;
            });
        }
    }
}

async fn emit_violation_event(
    broker: AnyBroker,
    plugin_id: String,
    attempted_name: String,
    reason: NamespaceViolationReason,
    mode: &'static str,
    rejected: bool,
) {
    // Use the broker abstraction's publish path; the existing
    // impl handles NATS / local fallback transparently. 2-second
    // total budget so a flaky broker can't block the plugin's
    // init for long.
    let topic = format!("plugin.lifecycle.{plugin_id}.namespace_violation");
    let mut payload = serde_json::json!({
        "plugin_id": plugin_id,
        "attempted_name": attempted_name,
        "reason": reason.as_str(),
        "mode": mode,
        "rejected": rejected,
    });
    if let Some(rp) = reason.reserved_prefix() {
        payload["reserved_prefix"] = serde_json::Value::String(rp.to_string());
    }
    let bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "namespace_violation payload serialize failed");
            return;
        }
    };
    let event = nexo_broker::Event::new(&topic, "plugin.namespace", payload);
    let publish = async move {
        if let Err(e) = broker.publish(&topic, event).await {
            tracing::warn!(error = %e, "namespace_violation publish failed");
        }
    };
    if tokio::time::timeout(BROKER_CONNECT_TIMEOUT, publish)
        .await
        .is_err()
    {
        tracing::warn!("namespace_violation publish timed out");
    }
    let _ = bytes; // serialization sanity-checked above; payload reused
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_llm::ToolDef;

    struct DummyHandler;
    #[async_trait::async_trait]
    impl ToolHandler for DummyHandler {
        async fn call(
            &self,
            _ctx: &crate::agent::context::AgentContext,
            _args: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::Value::Null)
        }
    }

    fn def(name: &str) -> ToolDef {
        ToolDef {
            name: name.to_string(),
            description: "x".into(),
            parameters: serde_json::json!({}),
        }
    }

    fn build_scoped(
        plugin_id: &str,
        expose: &[&str],
        mode: NamespaceEnforcement,
    ) -> (Arc<ScopedToolRegistry>, Arc<ToolRegistry>) {
        let inner = Arc::new(ToolRegistry::new());
        let exp: Vec<String> = expose.iter().map(|s| s.to_string()).collect();
        let scoped = Arc::new(ScopedToolRegistry::new(
            plugin_id.to_string(),
            &exp,
            inner.clone(),
            mode,
            None,
        ));
        (scoped, inner)
    }

    #[test]
    fn register_accepts_canonical_ext_name() {
        let (scoped, inner) = build_scoped("slack", &["slack_send"], NamespaceEnforcement::Strict);
        let result = scoped.register(def("ext_slack_slack_send"), DummyHandler);
        assert!(result.is_ok());
        assert!(inner.contains("ext_slack_slack_send"));
        assert!(scoped.drain_violations().is_empty());
    }

    #[test]
    fn register_accepts_bare_plugin_id_prefix() {
        let (scoped, inner) = build_scoped("slack", &["slack_send"], NamespaceEnforcement::Strict);
        let result = scoped.register(def("slack_send"), DummyHandler);
        assert!(result.is_ok());
        assert!(inner.contains("slack_send"));
    }

    #[test]
    fn register_rejects_unprefixed_tool() {
        let (scoped, _) = build_scoped("slack", &["slack_send"], NamespaceEnforcement::Strict);
        let err = scoped
            .register(def("send_message"), DummyHandler)
            .unwrap_err();
        assert_eq!(err.reason, NamespaceViolationReason::OutOfNamespace);
    }

    #[test]
    fn register_rejects_reserved_prefix_agent() {
        let (scoped, _) = build_scoped("slack", &["slack_send"], NamespaceEnforcement::Strict);
        let err = scoped.register(def("agent_route"), DummyHandler).unwrap_err();
        assert!(matches!(
            err.reason,
            NamespaceViolationReason::ReservedPrefix("agent_")
        ));
    }

    #[test]
    fn register_rejects_reserved_prefix_mcp() {
        let (scoped, _) = build_scoped("slack", &["slack_send"], NamespaceEnforcement::Strict);
        let err = scoped.register(def("mcp_call"), DummyHandler).unwrap_err();
        assert!(matches!(
            err.reason,
            NamespaceViolationReason::ReservedPrefix("mcp_")
        ));
    }

    #[test]
    fn register_rejects_collision_first_wins_in_warn_mode() {
        let (scoped, inner) = build_scoped("slack", &["slack_send"], NamespaceEnforcement::Warn);
        // Pre-populate via host-side path.
        inner.register(def("slack_send"), DummyHandler);
        let err = scoped
            .register(def("slack_send"), DummyHandler)
            .unwrap_err();
        assert_eq!(err.reason, NamespaceViolationReason::Collision);
    }

    #[test]
    fn register_rejects_tool_not_in_manifest_expose() {
        let (scoped, _) = build_scoped("slack", &["slack_send"], NamespaceEnforcement::Strict);
        let err = scoped
            .register(def("slack_unknown"), DummyHandler)
            .unwrap_err();
        assert_eq!(err.reason, NamespaceViolationReason::NotInExpose);
    }

    #[test]
    fn strict_mode_rejects_warn_mode_records_and_continues() {
        // Strict — registration fails outright.
        let (scoped_strict, inner_strict) =
            build_scoped("slack", &["slack_send"], NamespaceEnforcement::Strict);
        let err = scoped_strict
            .register(def("not_in_expose_at_all"), DummyHandler)
            .unwrap_err();
        assert_eq!(err.reason, NamespaceViolationReason::OutOfNamespace);
        assert!(!inner_strict.contains("not_in_expose_at_all"));
        let violations_strict = scoped_strict.drain_violations();
        assert_eq!(violations_strict.len(), 1);

        // Warn — namespace violation records but still attempts
        // register_if_absent.
        let (scoped_warn, inner_warn) =
            build_scoped("slack", &["slack_send"], NamespaceEnforcement::Warn);
        let result = scoped_warn.register(def("slack_unknown_tool"), DummyHandler);
        // NotInExpose is recorded but in Warn mode the tool was
        // still inserted (no collision case here).
        assert!(result.is_ok());
        assert!(inner_warn.contains("slack_unknown_tool"));
        let violations_warn = scoped_warn.drain_violations();
        assert_eq!(violations_warn.len(), 1);
        assert_eq!(
            violations_warn[0].reason,
            NamespaceViolationReason::NotInExpose
        );
    }

    #[test]
    fn empty_expose_rejects_every_register() {
        let (scoped, _) = build_scoped("slack", &[], NamespaceEnforcement::Strict);
        let err = scoped.register(def("slack_send"), DummyHandler).unwrap_err();
        assert_eq!(err.reason, NamespaceViolationReason::NotInExpose);
    }

    #[test]
    fn drain_violations_consumes_buffer() {
        let (scoped, _) = build_scoped("slack", &[], NamespaceEnforcement::Strict);
        let _ = scoped.register(def("slack_a"), DummyHandler);
        let _ = scoped.register(def("slack_b"), DummyHandler);
        assert_eq!(scoped.drain_violations().len(), 2);
        // Subsequent drain returns empty.
        assert!(scoped.drain_violations().is_empty());
    }

    #[test]
    fn reserved_prefix_takes_precedence_over_out_of_namespace() {
        let (scoped, _) = build_scoped("agentplugin", &[], NamespaceEnforcement::Strict);
        // `agent_` is reserved — even though the plugin id starts
        // with "agent", a name with literal `agent_` prefix is
        // rejected as ReservedPrefix BEFORE the namespace check.
        // (Note: id regex actually rejects "agent" colliding with
        // built-in names, but defense-in-depth here.)
        let err = scoped.register(def("agent_route"), DummyHandler).unwrap_err();
        assert!(matches!(
            err.reason,
            NamespaceViolationReason::ReservedPrefix("agent_")
        ));
    }
}
