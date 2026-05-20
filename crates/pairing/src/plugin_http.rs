//! Phase 81.33.b.real Stage 2 — daemon-side plugin HTTP proxy.
//!
//! Plugins declare `[plugin.http] mount_prefix = "/<prefix>"` in
//! their `nexo-plugin.toml`. The daemon builds a
//! [`PluginHttpRouter`] from every declaring plugin's manifest at
//! boot, and the HTTP handler checks the router on every incoming
//! request BEFORE the legacy hardcoded `/whatsapp/*` etc. paths.
//!
//! A match forwards the request to the plugin's subprocess via
//! broker JSON-RPC (`plugin.<id>.http.request`), reads the reply,
//! and writes it back to the TCP stream.
//!
//! ## Wire format
//!
//! See [`crate::plugin_http`] crate docs for the JSON shape.
//! Briefly:
//!
//! - request payload:
//!   `{ method, path, query, headers: [[k,v],…], body_base64 }`
//! - reply payload:
//!   `{ status, headers: [[k,v],…], body_base64 }`
//!
//! ## Limitations
//!
//! - No streaming responses (SSE, chunked transfer). Plugin must
//!   buffer the full response before replying.
//! - No WebSocket upgrades. WebSocket endpoints stay on
//!   `[plugin.http_server]` (plugin binds its own port).
//! - Body bytes are base64-encoded JSON; OK for HTML pages (≤100KB
//!   typical) but wasteful for large uploads.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use nexo_broker::{AnyBroker, BrokerHandle, Message};
use serde::{Deserialize, Serialize};
use serde_json::json;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Plugin-side reply parsed from the broker RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHttpResponse {
    /// HTTP status code (e.g. 200, 404, 503). Plugin must supply
    /// — daemon does NOT default.
    pub status: u16,
    /// Optional response headers. `Content-Type` recommended;
    /// daemon does NOT default-fill so the plugin owns the
    /// response shape completely.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Base64-encoded body bytes. Empty string = no body.
    #[serde(default)]
    pub body_base64: String,
}

impl PluginHttpResponse {
    /// Decode the response body. Returns empty vec on bad base64
    /// (logged at warn level).
    pub fn decoded_body(&self) -> Vec<u8> {
        if self.body_base64.is_empty() {
            return Vec::new();
        }
        base64::engine::general_purpose::STANDARD
            .decode(self.body_base64.as_bytes())
            .unwrap_or_default()
    }

    /// Pick a header value (case-insensitive). Returns first
    /// match.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Daemon-side route entry: which plugin owns which prefix +
/// per-plugin timeout.
#[derive(Debug, Clone)]
struct Route {
    mount_prefix: String,
    plugin_id: String,
    timeout: Duration,
}

/// Daemon-reserved path prefixes. Plugin registrations whose
/// `mount_prefix` collides with any of these are rejected so a
/// malicious or buggy plugin cannot hijack health/metrics/admin
/// surfaces.
///
/// Order: longest-first to match the router's matching order.
/// Comparison is exact-equality OR prefix-of-plugin: a plugin
/// asking for `/health/foo` would still shadow the daemon's
/// `/health` endpoint, so we reject prefixes that contain
/// a reserved path as their leading segment too.
pub const RESERVED_PREFIXES: &[&str] = &["/health", "/metrics", "/pair", "/admin", "/.well-known"];

/// Longest-prefix-first matcher: `register` keeps the route table
/// sorted so prefix matching is deterministic when prefixes nest
/// (`/api/v1` wins over `/api`).
/// Interior-mutable (Phase 97.1.γ 3b): the router lives behind an
/// `Arc` shared with the request loop, and hot plugin install /
/// uninstall mutates it through `&self` (RwLock) so HTTP routes go
/// live without a daemon restart.
#[derive(Debug, Default)]
pub struct PluginHttpRouter {
    routes: std::sync::RwLock<Vec<Route>>,
}

impl PluginHttpRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin's mount prefix. Duplicates are accepted
    /// (last-write-wins per `plugin_id`), so a plugin that
    /// re-registers (hot-spawn restart) replaces its own entry.
    ///
    /// Rejects registrations colliding with [`RESERVED_PREFIXES`]
    /// — daemon-internal paths (`/health`, `/metrics`, `/pair`,
    /// `/admin`, `/.well-known`) cannot be hijacked. Caller
    /// surfaces a warn-level log for rejected registrations so
    /// operators see the cause; the plugin's broker handler
    /// stays unhooked from those routes.
    pub fn register(
        &self,
        plugin_id: &str,
        mount_prefix: &str,
        timeout: Option<Duration>,
    ) -> Result<(), RouteRegistrationError> {
        for reserved in RESERVED_PREFIXES {
            if mount_prefix == *reserved || mount_prefix.starts_with(&format!("{reserved}/")) {
                return Err(RouteRegistrationError::Reserved {
                    requested: mount_prefix.to_string(),
                    reserved: (*reserved).to_string(),
                });
            }
        }
        let mut routes = self.routes.write().expect("plugin http routes lock");
        // Drop any existing entry for the same plugin_id (live
        // restart replaces).
        routes.retain(|r| r.plugin_id != plugin_id);
        routes.push(Route {
            mount_prefix: mount_prefix.to_string(),
            plugin_id: plugin_id.to_string(),
            timeout: timeout.unwrap_or(DEFAULT_TIMEOUT),
        });
        // Longest-prefix-first ordering — deterministic when prefixes
        // nest (`/api/v1` MUST win over `/api`).
        routes.sort_by(|a, b| {
            b.mount_prefix
                .len()
                .cmp(&a.mount_prefix.len())
                .then_with(|| a.plugin_id.cmp(&b.plugin_id))
        });
        Ok(())
    }

    /// Drop a plugin's route (hot uninstall / disable). Returns
    /// `true` when an entry was removed.
    pub fn unregister(&self, plugin_id: &str) -> bool {
        let mut routes = self.routes.write().expect("plugin http routes lock");
        let before = routes.len();
        routes.retain(|r| r.plugin_id != plugin_id);
        routes.len() != before
    }

    /// Match a request path. Returns `(plugin_id, timeout)` (owned,
    /// since the route table is behind a lock) if a route covers it;
    /// the caller forwards via [`forward_request`].
    pub fn match_path(&self, path: &str) -> Option<(String, Duration)> {
        let routes = self.routes.read().expect("plugin http routes lock");
        routes
            .iter()
            .find(|r| path.starts_with(&r.mount_prefix))
            .map(|r| (r.plugin_id.clone(), r.timeout))
    }

    pub fn is_empty(&self) -> bool {
        self.routes
            .read()
            .expect("plugin http routes lock")
            .is_empty()
    }
}

/// Forward a daemon-received HTTP request to the plugin via
/// broker JSON-RPC.
///
/// The caller has already parsed `method + full_path` from the
/// stream and snapshot any required headers / body bytes. This
/// helper does NOT touch the TCP stream — the caller writes the
/// response back.
///
/// On broker failure (timeout, transport error, malformed reply),
/// returns a typed error so the caller can render a 502/504.
pub async fn forward_request(
    broker: &AnyBroker,
    plugin_id: &str,
    method: &str,
    path: &str,
    query: &str,
    headers: &[(String, String)],
    body: &[u8],
    timeout: Duration,
) -> Result<PluginHttpResponse, PluginHttpForwardError> {
    let topic = format!("plugin.{plugin_id}.http.request");
    let body_b64 = base64::engine::general_purpose::STANDARD.encode(body);
    let payload = json!({
        "method": method,
        "path": path,
        "query": query,
        "headers": headers,
        "body_base64": body_b64,
    });
    let msg = Message::new(topic.clone(), payload);
    let reply = broker
        .request(&topic, msg, timeout)
        .await
        .map_err(|e| PluginHttpForwardError::Broker(e.to_string()))?;
    serde_json::from_value::<PluginHttpResponse>(reply.payload).map_err(|e| {
        PluginHttpForwardError::ParseReply(format!(
            "plugin {plugin_id} returned malformed http reply: {e}"
        ))
    })
}

/// Route registration error. Returned by
/// [`PluginHttpRouter::register`] when the requested
/// `mount_prefix` violates a reserved-prefix or validation rule.
#[derive(Debug, thiserror::Error)]
pub enum RouteRegistrationError {
    #[error("mount_prefix `{requested}` collides with daemon-reserved prefix `{reserved}`")]
    Reserved { requested: String, reserved: String },
}

/// Typed forwarder errors. Caller renders the right status
/// code:
/// - `Broker` → 504 Gateway Timeout (or 502 for non-timeout).
/// - `ParseReply` → 502 Bad Gateway (plugin contract violation).
#[derive(Debug, thiserror::Error)]
pub enum PluginHttpForwardError {
    #[error("broker error: {0}")]
    Broker(String),
    #[error("plugin reply parse error: {0}")]
    ParseReply(String),
}

/// Build a router from a slice of `(plugin_id, manifest)`. Skips
/// plugins whose manifest does not declare `[plugin.http]`.
pub fn build_router_from_handles(
    handles: &std::collections::BTreeMap<String, Arc<dyn DynPluginManifest>>,
) -> PluginHttpRouter {
    let router = PluginHttpRouter::new();
    for (plugin_id, handle) in handles.iter() {
        if let Some(section) = handle.http_section() {
            let _ = router.register(plugin_id, &section.mount_prefix, section.timeout);
        }
    }
    router
}

/// Type-erased view of a plugin's manifest sufficient for router
/// construction. The real `NexoPlugin` trait lives in nexo-core;
/// pulling it into this crate would create a circular dep. The
/// caller (daemon) walks `wire.plugin_handles` and wraps each
/// handle in a small impl of this trait OR — simpler — passes a
/// `Vec<(plugin_id, mount_prefix, timeout)>` directly to
/// [`PluginHttpRouter::register`] in a loop.
///
/// We provide the trait as documentation for the contract; the
/// daemon uses the direct `register()` loop in practice.
pub trait DynPluginManifest: Send + Sync {
    fn http_section(&self) -> Option<HttpSectionView>;
}

/// View struct mirroring
/// [`nexo_plugin_manifest::http::PluginHttpSection`] but without
/// the cross-crate dep (router crate already depends on
/// nexo-plugin-manifest so this is mostly cosmetic — kept here so
/// downstream alternative impls can avoid the dep if needed).
#[derive(Debug, Clone)]
pub struct HttpSectionView {
    pub mount_prefix: String,
    pub timeout: Option<Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_path_uses_longest_prefix_first() {
        let r = PluginHttpRouter::new();
        r.register("plugin_short", "/api", None).unwrap();
        r.register("plugin_long", "/api/v1", None).unwrap();
        let (id, _) = r.match_path("/api/v1/users").expect("matches");
        assert_eq!(id, "plugin_long");
    }

    #[test]
    fn match_path_falls_back_to_shorter_prefix() {
        let r = PluginHttpRouter::new();
        r.register("plugin_short", "/api", None).unwrap();
        r.register("plugin_long", "/api/v1", None).unwrap();
        let (id, _) = r.match_path("/api/v2/users").expect("matches");
        assert_eq!(id, "plugin_short");
    }

    #[test]
    fn match_path_returns_none_for_no_match() {
        let r = PluginHttpRouter::new();
        r.register("plugin", "/whatsapp", None).unwrap();
        assert!(r.match_path("/foo").is_none());
        assert!(r.match_path("/").is_none());
    }

    #[test]
    fn register_replaces_existing_entry_for_same_plugin() {
        let r = PluginHttpRouter::new();
        r.register("plugin", "/old", None).unwrap();
        r.register("plugin", "/new", None).unwrap();
        assert!(r.match_path("/old").is_none());
        assert!(r.match_path("/new").is_some());
    }

    #[test]
    fn register_applies_custom_timeout() {
        let r = PluginHttpRouter::new();
        r.register("plugin", "/slow", Some(Duration::from_secs(120)))
            .unwrap();
        let (_, t) = r.match_path("/slow/foo").unwrap();
        assert_eq!(t, Duration::from_secs(120));
    }

    #[test]
    fn register_default_timeout_when_none() {
        let r = PluginHttpRouter::new();
        r.register("plugin", "/fast", None).unwrap();
        let (_, t) = r.match_path("/fast/foo").unwrap();
        assert_eq!(t, DEFAULT_TIMEOUT);
    }

    #[test]
    fn hot_register_and_unregister_through_shared_ref() {
        // Phase 97.1.γ 3b — interior mutability: register + unregister
        // through a shared `&self` (the daemon holds an `Arc`).
        let r = std::sync::Arc::new(PluginHttpRouter::new());
        r.register("browser", "/browser", None).unwrap();
        assert!(r.match_path("/browser/screenshot").is_some());
        assert!(r.unregister("browser"));
        assert!(r.match_path("/browser/screenshot").is_none());
        assert!(!r.unregister("browser"), "second remove is a no-op");
    }

    #[test]
    fn register_rejects_reserved_prefixes() {
        let r = PluginHttpRouter::new();
        for reserved in RESERVED_PREFIXES {
            let result = r.register("evil_plugin", reserved, None);
            assert!(
                matches!(result, Err(RouteRegistrationError::Reserved { .. })),
                "expected reservation rejection for `{reserved}`, got {result:?}",
            );
        }
    }

    #[test]
    fn register_rejects_subpath_of_reserved() {
        let r = PluginHttpRouter::new();
        // `/health/foo` shadows the daemon's `/health` endpoint
        // if matched first — must be rejected.
        let result = r.register("evil_plugin", "/health/foo", None);
        assert!(matches!(
            result,
            Err(RouteRegistrationError::Reserved { .. })
        ));
    }

    #[test]
    fn register_accepts_prefixes_that_only_share_substring_with_reserved() {
        let r = PluginHttpRouter::new();
        // `/healthy` is NOT a subpath of `/health` — `/health/` is
        // the prefix that would shadow; `/healthy` is a sibling.
        assert!(r.register("plugin", "/healthy", None).is_ok());
        assert!(r.register("plugin2", "/metrics-aggregator", None).is_ok());
    }

    #[test]
    fn plugin_http_response_decodes_body() {
        let response = PluginHttpResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html".into())],
            body_base64: base64::engine::general_purpose::STANDARD.encode("<html/>"),
        };
        assert_eq!(response.decoded_body(), b"<html/>");
        assert_eq!(response.header("content-type"), Some("text/html"));
        assert_eq!(response.header("CONTENT-TYPE"), Some("text/html"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forward_request_returns_broker_error_when_no_subscriber() {
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let result = forward_request(
            &broker,
            "plugin",
            "GET",
            "/whatsapp/pair",
            "",
            &[],
            &[],
            Duration::from_millis(100),
        )
        .await;
        assert!(matches!(result, Err(PluginHttpForwardError::Broker(_))));
    }
}
