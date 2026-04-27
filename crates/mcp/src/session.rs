//! Phase 12.4 — session-scoped MCP runtime.
//!
//! Owns one `StdioMcpClient` per configured server. The catalog lives
//! outside this crate (nexo-core builds it from the runtime's client
//! snapshot) to keep `nexo-mcp` independent of `ToolRegistry`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::join_all;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::client::StdioMcpClient;
use crate::client_trait::McpClient;
use crate::errors::McpError;
use crate::events::ClientEvent;
use crate::http::HttpMcpClient;
use crate::http::HttpMcpOptions;
use crate::resource_cache::ResourceCache;
use crate::runtime_config::McpServerRuntimeConfig;
use crate::sampling::SamplingProvider;
use crate::types::McpToolResult;

/// Time window that collapses bursts of list-changed notifications
/// (tools or resources) into a single callback fire. Short enough that
/// users don't notice but long enough to absorb SSE reconnect
/// re-emissions.
const EVENT_DEBOUNCE: Duration = Duration::from_millis(200);

fn is_tools_changed(e: &ClientEvent) -> bool {
    matches!(e, ClientEvent::ToolsListChanged)
}

fn is_resources_changed(e: &ClientEvent) -> bool {
    matches!(e, ClientEvent::ResourcesListChanged)
}

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

#[derive(Debug, Error)]
pub enum RuntimeCallError {
    #[error("runtime disposed")]
    Disposed,
    #[error("unknown server `{0}`")]
    UnknownServer(String),
    #[error(transparent)]
    Mcp(#[from] McpError),
}

pub struct SessionMcpRuntime {
    session_id: Uuid,
    config_fingerprint: String,
    created_at: DateTime<Utc>,
    last_used_at: Arc<RwLock<DateTime<Utc>>>,
    clients: Arc<RwLock<HashMap<String, Arc<dyn McpClient>>>>,
    disposed: Arc<AtomicBool>,
    /// Phase 12.8 — spawned debounce tasks, one per client per
    /// `on_tools_changed` registration. Aborted on dispose.
    reload_tasks: Mutex<Vec<JoinHandle<()>>>,
    /// Phase 12.5 follow-up — shared `resources/read` cache. Disabled
    /// cache is a cheap no-op so every runtime always owns one.
    resource_cache: Arc<ResourceCache>,
    /// Phase 12.5 follow-up — opt-in URI scheme allowlist. Empty means
    /// permissive. Passed through to `McpResourceReadTool` when the
    /// catalog registers resource meta-tools.
    resource_uri_allowlist: Arc<Vec<String>>,
}

impl SessionMcpRuntime {
    pub fn new(
        session_id: Uuid,
        config_fingerprint: String,
        clients: HashMap<String, Arc<dyn McpClient>>,
    ) -> Self {
        let now = Utc::now();
        Self {
            session_id,
            config_fingerprint,
            created_at: now,
            last_used_at: Arc::new(RwLock::new(now)),
            clients: Arc::new(RwLock::new(clients)),
            disposed: Arc::new(AtomicBool::new(false)),
            reload_tasks: Mutex::new(Vec::new()),
            resource_cache: Arc::new(ResourceCache::disabled()),
            resource_uri_allowlist: Arc::new(Vec::new()),
        }
    }

    /// Phase 12.5 follow-up — attach an (already-configured) resource
    /// cache. Builder style so the manager can pass the cache forged from
    /// `McpRuntimeConfig.resource_cache`.
    pub fn with_resource_cache(mut self, cache: Arc<ResourceCache>) -> Self {
        self.resource_cache = cache;
        self
    }

    pub fn with_resource_uri_allowlist(mut self, list: Arc<Vec<String>>) -> Self {
        self.resource_uri_allowlist = list;
        self
    }

    /// Shared cache used by `resources/read` tools. Disabled cache acts
    /// as a cheap no-op.
    pub fn resource_cache(&self) -> Arc<ResourceCache> {
        Arc::clone(&self.resource_cache)
    }

    pub fn resource_uri_allowlist(&self) -> Arc<Vec<String>> {
        Arc::clone(&self.resource_uri_allowlist)
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn config_fingerprint(&self) -> &str {
        &self.config_fingerprint
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn last_used_at(&self) -> DateTime<Utc> {
        *self.last_used_at.read().unwrap()
    }

    pub fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::Acquire)
    }

    /// Snapshot of currently-connected clients. Useful for catalog builds.
    pub fn clients(&self) -> Vec<(String, Arc<dyn McpClient>)> {
        self.clients
            .read()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Mark the runtime as recently used; call this from wrappers (catalog
    /// build, registry integration) that should count as activity.
    pub fn touch(&self) {
        *self.last_used_at.write().unwrap() = Utc::now();
    }

    /// Signal that the catalog view of this session is stale. The runtime
    /// itself caches nothing (catalog lives in nexo-core's
    /// `ToolRegistry`), so this just bumps `last_used_at` and logs. The
    /// real reload path is `on_tools_changed`, which fires a user-supplied
    /// closure that rebuilds the registry view.
    pub fn invalidate_catalog(&self) {
        self.touch();
        tracing::debug!(session = %self.session_id, "catalog invalidation requested");
    }

    /// Phase 12.8 — register a closure that fires once per server when the
    /// server pushes `notifications/tools/list_changed`. Rapid bursts of
    /// notifications collapse into a single callback within a 200 ms
    /// window. Safe to call multiple times; each call spawns its own set
    /// of debounce tasks.
    pub fn on_tools_changed<F>(&self, f: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        self.spawn_event_listeners(Arc::new(f), is_tools_changed);
    }

    /// Phase 12.8 — symmetric to `on_tools_changed` but fires on
    /// `notifications/resources/list_changed`. Same 200 ms debounce and
    /// same dispose semantics.
    pub fn on_resources_changed<F>(&self, f: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        self.spawn_event_listeners(Arc::new(f), is_resources_changed);
    }

    fn spawn_event_listeners(
        &self,
        cb: Arc<dyn Fn(String) + Send + Sync + 'static>,
        matcher: fn(&ClientEvent) -> bool,
    ) {
        if self.is_disposed() {
            return;
        }
        let clients: Vec<(String, Arc<dyn McpClient>)> = self
            .clients
            .read()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut handles = self.reload_tasks.lock().unwrap();
        for (server_id, client) in clients {
            let rx = client.subscribe_events();
            let disposed = self.disposed.clone();
            let cb = cb.clone();
            let handle = tokio::spawn(debounce_event(rx, server_id, matcher, cb, disposed));
            handles.push(handle);
        }
    }

    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        args: Value,
    ) -> Result<McpToolResult, RuntimeCallError> {
        if self.is_disposed() {
            return Err(RuntimeCallError::Disposed);
        }
        let client = self
            .clients
            .read()
            .unwrap()
            .get(server)
            .cloned()
            .ok_or_else(|| RuntimeCallError::UnknownServer(server.to_string()))?;
        self.touch();
        Ok(client.call_tool(tool, args).await?)
    }

    /// Best-effort graceful teardown. Idempotent.
    pub async fn dispose(&self) {
        self.dispose_with_reason("client shutdown").await
    }

    /// Variant that threads `reason` into the `notifications/cancelled`
    /// each live client emits before shutting down.
    pub async fn dispose_with_reason(&self, reason: &str) {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        // Phase 9.2 follow-up — fire session lifecycle metrics exactly
        // once per live session (the `swap(true)` above guards re-entry).
        let lifetime_secs = (Utc::now() - self.created_at).num_seconds().max(0) as u64;
        crate::telemetry::dec_sessions_active();
        crate::telemetry::inc_sessions_disposed(reason);
        crate::telemetry::observe_session_lifetime_secs(lifetime_secs);

        // Abort reload watchers first so they stop holding client Arcs.
        {
            let mut tasks = self.reload_tasks.lock().unwrap();
            for h in tasks.drain(..) {
                h.abort();
            }
        }
        let clients: Vec<Arc<dyn McpClient>> = {
            let mut map = self.clients.write().unwrap();
            let vs: Vec<Arc<dyn McpClient>> = map.values().cloned().collect();
            map.clear();
            vs
        };
        let reason = reason.to_string();
        let shutdowns = clients.iter().map(|c| {
            let c = c.clone();
            let reason = reason.clone();
            async move { c.shutdown_with_reason(&reason).await }
        });
        let _ = join_all(shutdowns).await;
    }
}

/// Loop: wait for an event that satisfies `matcher`, then absorb
/// additional events for `EVENT_DEBOUNCE` before firing the callback once.
/// Exits when the broadcast channel is closed or the session is disposed.
/// Generic over matcher so `on_tools_changed` and `on_resources_changed`
/// share the same machinery.
async fn debounce_event(
    mut rx: broadcast::Receiver<ClientEvent>,
    server_id: String,
    matcher: fn(&ClientEvent) -> bool,
    cb: Arc<dyn Fn(String) + Send + Sync + 'static>,
    disposed: Arc<AtomicBool>,
) {
    loop {
        if disposed.load(Ordering::Acquire) {
            return;
        }
        let first = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Closed) => return,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
        };
        if !matcher(&first) {
            continue;
        }
        let deadline = tokio::time::Instant::now() + EVENT_DEBOUNCE;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                next = rx.recv() => {
                    match next {
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    }
                }
            }
        }
        if disposed.load(Ordering::Acquire) {
            return;
        }
        cb(server_id.clone());
    }
}

/// Connect every configured server in parallel. Failures are logged at
/// `warn` and the offending server is simply absent from the returned map
/// (consistent with 12.3 fail-open semantics).
pub(crate) async fn connect_all_with_sampling(
    configs: &[McpServerRuntimeConfig],
    sampling_provider: Option<Arc<dyn SamplingProvider>>,
) -> HashMap<String, Arc<dyn McpClient>> {
    let futures = configs.iter().map(|cfg| {
        let cfg = cfg.clone();
        let sampling_provider = sampling_provider.clone();
        async move { connect_one(cfg, sampling_provider).await }
    });
    let results = join_all(futures).await;
    results.into_iter().flatten().collect()
}

async fn connect_one(
    cfg: McpServerRuntimeConfig,
    sampling_provider: Option<Arc<dyn SamplingProvider>>,
) -> Option<(String, Arc<dyn McpClient>)> {
    let name = cfg.name().to_string();
    match cfg {
        McpServerRuntimeConfig::Stdio(inner) => {
            match StdioMcpClient::connect_with_sampling(inner, sampling_provider).await {
                Ok(c) => Some((name, Arc::new(c) as Arc<dyn McpClient>)),
                Err(e) => {
                    tracing::warn!(mcp = %name, error = %e, "mcp stdio connect failed");
                    None
                }
            }
        }
        McpServerRuntimeConfig::Http {
            url,
            transport,
            headers,
            connect_timeout,
            initialize_timeout,
            call_timeout,
            shutdown_grace,
            log_level,
            ..
        } => {
            let mut header_map = HeaderMap::new();
            for (k, v) in &headers {
                let Ok(name) = HeaderName::from_bytes(k.as_bytes()) else {
                    tracing::warn!(mcp = %name, key = %k, "invalid header name; skipping");
                    continue;
                };
                let Ok(val) = HeaderValue::from_str(v) else {
                    tracing::warn!(mcp = %name, key = %k, "invalid header value; skipping");
                    continue;
                };
                header_map.insert(name, val);
            }
            let opts = HttpMcpOptions {
                connect_timeout,
                initialize_timeout,
                call_timeout,
                shutdown_grace,
                log_level,
                ..Default::default()
            };
            match HttpMcpClient::connect(name.clone(), url, transport, header_map, opts).await {
                Ok(c) => Some((name, Arc::new(c) as Arc<dyn McpClient>)),
                Err(e) => {
                    tracing::warn!(mcp = %name, error = %e, "mcp http connect failed");
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::McpClientState;
    use crate::types::McpContent;
    use crate::types::{McpCapabilities, McpServerInfo, McpTool, McpToolResult};
    use async_trait::async_trait;

    struct MockEventClient {
        name: String,
        info: McpServerInfo,
        caps: McpCapabilities,
        tx: broadcast::Sender<ClientEvent>,
    }

    impl MockEventClient {
        fn new(name: &str) -> (Self, broadcast::Sender<ClientEvent>) {
            let (tx, _) = broadcast::channel(32);
            let client = Self {
                name: name.into(),
                info: McpServerInfo {
                    name: name.into(),
                    version: "0".into(),
                },
                caps: McpCapabilities::default(),
                tx: tx.clone(),
            };
            (client, tx)
        }
    }

    #[async_trait]
    impl McpClient for MockEventClient {
        fn name(&self) -> &str {
            &self.name
        }
        fn server_info(&self) -> &McpServerInfo {
            &self.info
        }
        fn capabilities(&self) -> &McpCapabilities {
            &self.caps
        }
        fn protocol_version(&self) -> &str {
            "2024-11-05"
        }
        fn state(&self) -> McpClientState {
            McpClientState::Ready
        }
        async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
            Ok(vec![])
        }
        async fn call_tool(&self, _n: &str, _a: Value) -> Result<McpToolResult, McpError> {
            Ok(McpToolResult {
                content: Vec::<McpContent>::new(),
                is_error: false,
                structured_content: None,
            })
        }
        async fn shutdown(&self) {}
        fn subscribe_events(&self) -> broadcast::Receiver<ClientEvent> {
            self.tx.subscribe()
        }
    }

    #[test]
    fn dispose_is_idempotent() {
        let rt = SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), HashMap::new());
        let rt = Arc::new(rt);
        let rt2 = rt.clone();
        // Run on a runtime because `dispose` is async.
        let tok = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tok.block_on(async move {
            rt2.dispose().await;
            rt2.dispose().await; // second call must not panic
            assert!(rt2.is_disposed());
        });
    }

    #[tokio::test]
    async fn call_tool_on_disposed_returns_disposed() {
        let rt = SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), HashMap::new());
        rt.dispose().await;
        let err = rt
            .call_tool("nope", "tool", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, RuntimeCallError::Disposed));
    }

    #[tokio::test]
    async fn on_tools_changed_fires_callback_once() {
        let (mock, tx) = MockEventClient::new("srv-a");
        let mut map: HashMap<String, Arc<dyn McpClient>> = HashMap::new();
        map.insert("srv-a".into(), Arc::new(mock));
        let rt = Arc::new(SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), map));

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_server: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let counter = counter.clone();
            let seen_server = seen_server.clone();
            rt.on_tools_changed(move |sid| {
                counter.fetch_add(1, Ordering::SeqCst);
                *seen_server.lock().unwrap() = Some(sid);
            });
        }

        // Give the debounce task a tick to subscribe.
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Fire 5 notifications in rapid succession.
        for _ in 0..5 {
            let _ = tx.send(ClientEvent::ToolsListChanged);
        }
        // Wait past debounce window.
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(seen_server.lock().unwrap().as_deref(), Some("srv-a"));
    }

    #[tokio::test]
    async fn on_tools_changed_ignores_other_events() {
        let (mock, tx) = MockEventClient::new("srv-b");
        let mut map: HashMap<String, Arc<dyn McpClient>> = HashMap::new();
        map.insert("srv-b".into(), Arc::new(mock));
        let rt = Arc::new(SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), map));

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let counter = counter.clone();
            rt.on_tools_changed(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = tx.send(ClientEvent::ResourcesListChanged);
        let _ = tx.send(ClientEvent::Other("notifications/progress".into()));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn on_resources_changed_fires_callback_once() {
        let (mock, tx) = MockEventClient::new("srv-r");
        let mut map: HashMap<String, Arc<dyn McpClient>> = HashMap::new();
        map.insert("srv-r".into(), Arc::new(mock));
        let rt = Arc::new(SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), map));

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let counter = counter.clone();
            let seen = seen.clone();
            rt.on_resources_changed(move |sid| {
                counter.fetch_add(1, Ordering::SeqCst);
                *seen.lock().unwrap() = Some(sid);
            });
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        for _ in 0..5 {
            let _ = tx.send(ClientEvent::ResourcesListChanged);
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(seen.lock().unwrap().as_deref(), Some("srv-r"));
    }

    #[tokio::test]
    async fn on_resources_changed_ignores_tools_events() {
        let (mock, tx) = MockEventClient::new("srv-r2");
        let mut map: HashMap<String, Arc<dyn McpClient>> = HashMap::new();
        map.insert("srv-r2".into(), Arc::new(mock));
        let rt = Arc::new(SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), map));

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let counter = counter.clone();
            rt.on_resources_changed(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = tx.send(ClientEvent::ToolsListChanged);
        let _ = tx.send(ClientEvent::Other("notifications/progress".into()));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn tools_and_resources_callbacks_are_independent() {
        let (mock, tx) = MockEventClient::new("srv-dual");
        let mut map: HashMap<String, Arc<dyn McpClient>> = HashMap::new();
        map.insert("srv-dual".into(), Arc::new(mock));
        let rt = Arc::new(SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), map));

        let tools = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resources = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let tools = tools.clone();
            rt.on_tools_changed(move |_| {
                tools.fetch_add(1, Ordering::SeqCst);
            });
            let resources = resources.clone();
            rt.on_resources_changed(move |_| {
                resources.fetch_add(1, Ordering::SeqCst);
            });
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = tx.send(ClientEvent::ToolsListChanged);
        let _ = tx.send(ClientEvent::ResourcesListChanged);
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(tools.load(Ordering::SeqCst), 1);
        assert_eq!(resources.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn call_tool_unknown_server_errors() {
        let rt = SessionMcpRuntime::new(Uuid::new_v4(), "fp".into(), HashMap::new());
        let err = rt
            .call_tool("nope", "tool", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, RuntimeCallError::UnknownServer(_)));
    }
}
