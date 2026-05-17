//! Phase 81.33.b.real Stage 4 — daemon-side plugin admin RPC
//! router.
//!
//! Plugins declare `[plugin.admin] method_prefix = "nexo/admin/<id>/"`
//! in their `nexo-plugin.toml`. The admin dispatcher checks the
//! router for every incoming method; matches forward via broker
//! JSON-RPC to the plugin's subprocess. Replaces the previous
//! `.with_<plugin>_handle(Arc<dyn XxxHandle>)` typed builder
//! pattern.
//!
//! See [`crate::plugin_http`] for the sibling HTTP router that
//! shares this design pattern (longest-prefix-first matching +
//! broker-RPC forwarding + reserved-prefix safety net).

use std::time::Duration;

use nexo_broker::{AnyBroker, BrokerHandle, Message};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Method prefixes reserved for daemon-internal admin handlers.
/// Plugin registrations colliding with any of these are rejected
/// — the daemon's agents/credentials/pairing/etc. surfaces
/// cannot be hijacked.
///
/// Comparison: exact prefix-equality or sub-prefix containment.
pub const RESERVED_ADMIN_PREFIXES: &[&str] = &[
    "nexo/admin/agents/",
    "nexo/admin/credentials/",
    "nexo/admin/pairing/",
    "nexo/admin/llm/",
    "nexo/admin/channels/",
    "nexo/admin/tenants/",
    "nexo/admin/memory/",
    "nexo/admin/sessions/",
    "nexo/admin/snapshots/",
    "nexo/admin/policy/",
];

/// Plugin reply shape. Mirrors the dispatcher's typed
/// `AdminRpcResult` so the daemon can render the right status
/// without per-plugin parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAdminResponse {
    /// `true` = success; `false` = error.
    pub ok: bool,
    /// Result payload when `ok = true`. Caller deserializes into
    /// the typed response shape (per `nexo-tool-meta::admin`).
    #[serde(default)]
    pub result: Value,
    /// Error message when `ok = false`.
    #[serde(default)]
    pub error: String,
}

#[derive(Debug, Clone)]
struct Route {
    method_prefix: String,
    broker_topic_prefix: String,
    plugin_id: String,
    timeout: Duration,
}

/// Longest-prefix-first matcher with interior mutability so the
/// daemon can construct an empty router at admin-bootstrap time
/// and populate it AFTER `wire_plugin_registry` returns the
/// plugin handles.
#[derive(Debug, Default)]
pub struct PluginAdminRouter {
    routes: std::sync::RwLock<Vec<Route>>,
}

impl PluginAdminRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin's admin prefix. Rejects registrations
    /// colliding with [`RESERVED_ADMIN_PREFIXES`]. Duplicate
    /// `plugin_id` overrides previous entries (hot-spawn restart).
    pub fn register(
        &self,
        plugin_id: &str,
        method_prefix: &str,
        broker_topic_prefix: &str,
        timeout: Option<Duration>,
    ) -> Result<(), AdminRouteRegistrationError> {
        for reserved in RESERVED_ADMIN_PREFIXES {
            if method_prefix == *reserved
                || method_prefix.starts_with(reserved)
                || reserved.starts_with(method_prefix)
            {
                return Err(AdminRouteRegistrationError::Reserved {
                    requested: method_prefix.to_string(),
                    reserved: (*reserved).to_string(),
                });
            }
        }
        let mut routes = self.routes.write().expect("router lock poisoned");
        routes.retain(|r| r.plugin_id != plugin_id);
        routes.push(Route {
            method_prefix: method_prefix.to_string(),
            broker_topic_prefix: broker_topic_prefix.to_string(),
            plugin_id: plugin_id.to_string(),
            timeout: timeout.unwrap_or(DEFAULT_TIMEOUT),
        });
        routes.sort_by(|a, b| {
            b.method_prefix
                .len()
                .cmp(&a.method_prefix.len())
                .then_with(|| a.plugin_id.cmp(&b.plugin_id))
        });
        Ok(())
    }

    /// Match an admin method against registered prefixes.
    /// Returns the dispatch metadata needed by
    /// [`forward_request`]. The returned [`MatchInfo`] is owned
    /// (not borrowing the router) so the caller can drop the
    /// router lock immediately.
    pub fn match_method(&self, method: &str) -> Option<MatchInfo> {
        let routes = self.routes.read().expect("router lock poisoned");
        routes
            .iter()
            .find(|r| method.starts_with(&r.method_prefix))
            .map(|r| MatchInfo {
                plugin_id: r.plugin_id.clone(),
                broker_topic_prefix: r.broker_topic_prefix.clone(),
                method_prefix: r.method_prefix.clone(),
                timeout: r.timeout,
            })
    }

    pub fn is_empty(&self) -> bool {
        self.routes.read().expect("router lock poisoned").is_empty()
    }
}

/// Output of [`PluginAdminRouter::match_method`]. Owned strings
/// so the caller can drop the router lock immediately and the
/// resulting forward call does not hold the lock across an
/// async broker round-trip.
#[derive(Debug, Clone)]
pub struct MatchInfo {
    pub plugin_id: String,
    pub broker_topic_prefix: String,
    pub method_prefix: String,
    pub timeout: Duration,
}

/// Translate the trailing portion of an admin method into the
/// broker subject suffix. Replaces `/` with `.` so
/// `nexo/admin/whatsapp/bot/list` with prefix `nexo/admin/whatsapp/`
/// → `bot/list` → `bot.list`.
pub fn method_to_broker_suffix(method: &str, prefix: &str) -> String {
    method
        .strip_prefix(prefix)
        .unwrap_or(method)
        .replace('/', ".")
}

/// Forward an admin method invocation to the plugin via broker
/// JSON-RPC. Caller passes pre-parsed JSON params + parses the
/// returned [`PluginAdminResponse`] into the typed shape.
pub async fn forward_request(
    broker: &AnyBroker,
    info: MatchInfo,
    method: &str,
    params: Value,
) -> Result<PluginAdminResponse, PluginAdminForwardError> {
    let suffix = method_to_broker_suffix(method, &info.method_prefix);
    let topic = format!("{}.{suffix}", info.broker_topic_prefix);
    let payload = json!({ "method": method, "params": params });
    let msg = Message::new(topic.clone(), payload);
    let reply = broker
        .request(&topic, msg, info.timeout)
        .await
        .map_err(|e| PluginAdminForwardError::Broker(e.to_string()))?;
    serde_json::from_value::<PluginAdminResponse>(reply.payload).map_err(|e| {
        PluginAdminForwardError::ParseReply(format!(
            "plugin {} returned malformed admin reply: {e}",
            info.plugin_id
        ))
    })
}

/// Route registration error. Returned by
/// [`PluginAdminRouter::register`].
#[derive(Debug, thiserror::Error)]
pub enum AdminRouteRegistrationError {
    #[error("method_prefix `{requested}` collides with daemon-reserved prefix `{reserved}`")]
    Reserved { requested: String, reserved: String },
}

/// Typed forwarder errors.
#[derive(Debug, thiserror::Error)]
pub enum PluginAdminForwardError {
    #[error("broker error: {0}")]
    Broker(String),
    #[error("plugin reply parse error: {0}")]
    ParseReply(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_method_uses_longest_prefix_first() {
        let r = PluginAdminRouter::new();
        r.register("wa", "nexo/admin/whatsapp/", "plugin.whatsapp.admin", None)
            .unwrap();
        r.register(
            "wa_bot",
            "nexo/admin/whatsapp/bot/",
            "plugin.whatsapp.bot",
            None,
        )
        .unwrap();
        let m = r
            .match_method("nexo/admin/whatsapp/bot/list")
            .expect("matches");
        assert_eq!(m.plugin_id, "wa_bot");
    }

    #[test]
    fn match_method_falls_back_to_shorter_prefix() {
        let r = PluginAdminRouter::new();
        r.register("wa", "nexo/admin/whatsapp/", "plugin.whatsapp.admin", None)
            .unwrap();
        r.register(
            "wa_bot",
            "nexo/admin/whatsapp/bot/",
            "plugin.whatsapp.bot",
            None,
        )
        .unwrap();
        let m = r
            .match_method("nexo/admin/whatsapp/session/qr")
            .expect("matches");
        assert_eq!(m.plugin_id, "wa");
    }

    #[test]
    fn match_method_returns_none_on_miss() {
        let r = PluginAdminRouter::new();
        r.register("wa", "nexo/admin/whatsapp/", "plugin.whatsapp.admin", None)
            .unwrap();
        assert!(r.match_method("nexo/admin/agents/list").is_none());
    }

    #[test]
    fn register_rejects_reserved_prefixes() {
        let r = PluginAdminRouter::new();
        for reserved in RESERVED_ADMIN_PREFIXES {
            let result = r.register("evil", reserved, "plugin.evil", None);
            assert!(
                matches!(result, Err(AdminRouteRegistrationError::Reserved { .. })),
                "expected rejection for `{reserved}`",
            );
        }
    }

    #[test]
    fn register_rejects_subpath_of_reserved() {
        let r = PluginAdminRouter::new();
        let result = r.register("evil", "nexo/admin/agents/sneaky/", "plugin.evil", None);
        assert!(matches!(
            result,
            Err(AdminRouteRegistrationError::Reserved { .. })
        ));
    }

    #[test]
    fn register_rejects_super_prefix_of_reserved() {
        // A plugin asking for `nexo/admin/age/` would shadow the
        // daemon's `nexo/admin/agents/` namespace if matched
        // first (because `nexo/admin/age/` is a SHORTER prefix
        // that the dispatcher would compare against).
        let r = PluginAdminRouter::new();
        let result = r.register("evil", "nexo/admin/", "plugin.evil", None);
        assert!(matches!(
            result,
            Err(AdminRouteRegistrationError::Reserved { .. })
        ));
    }

    #[test]
    fn method_to_broker_suffix_replaces_slashes_with_dots() {
        let suffix =
            method_to_broker_suffix("nexo/admin/whatsapp/bot/list", "nexo/admin/whatsapp/");
        assert_eq!(suffix, "bot.list");
    }

    #[test]
    fn method_to_broker_suffix_falls_back_when_prefix_missing() {
        // Defensive — should never happen if router matched, but
        // confirms the function doesn't panic.
        let suffix =
            method_to_broker_suffix("nexo/admin/whatsapp/bot/list", "nexo/admin/telegram/");
        assert_eq!(suffix, "nexo.admin.whatsapp.bot.list");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forward_request_returns_broker_error_with_no_subscriber() {
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let router = PluginAdminRouter::new();
        router
            .register(
                "wa",
                "nexo/admin/whatsapp/",
                "plugin.whatsapp.admin",
                Some(Duration::from_millis(100)),
            )
            .unwrap();
        let info = router.match_method("nexo/admin/whatsapp/bot/list").unwrap();
        let result =
            forward_request(&broker, info, "nexo/admin/whatsapp/bot/list", json!({})).await;
        assert!(matches!(result, Err(PluginAdminForwardError::Broker(_))));
    }
}
