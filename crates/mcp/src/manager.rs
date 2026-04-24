//! Phase 12.4 — `McpRuntimeManager`: one `SessionMcpRuntime` per
//! `session_id` with in-flight dedup, lazy fingerprint-based invalidation,
//! and a background idle reap task.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use futures::future::{BoxFuture, FutureExt, Shared};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::runtime_config::{McpRuntimeConfig, McpServerRuntimeConfig};
use crate::session::{connect_all, SessionMcpRuntime};

type SharedRuntime = Shared<BoxFuture<'static, Arc<SessionMcpRuntime>>>;

pub struct McpRuntimeManager {
    config: Arc<RwLock<McpRuntimeConfig>>,
    runtimes: Arc<DashMap<Uuid, Arc<SessionMcpRuntime>>>,
    in_flight: Arc<Mutex<HashMap<Uuid, SharedRuntime>>>,
    shutdown: CancellationToken,
    reap_handle: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl McpRuntimeManager {
    /// Build the manager and spawn its reap task. Return an `Arc` so the
    /// reap task and callers share the same instance.
    pub fn new(config: McpRuntimeConfig) -> Arc<Self> {
        let shutdown = CancellationToken::new();
        let manager = Arc::new(Self {
            config: Arc::new(RwLock::new(config.clone())),
            runtimes: Arc::new(DashMap::new()),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            shutdown: shutdown.clone(),
            reap_handle: std::sync::Mutex::new(None),
        });
        let handle = tokio::spawn(reap_loop(
            manager.clone(),
            config.idle_reap_interval,
            config.session_ttl,
            shutdown,
        ));
        *manager.reap_handle.lock().unwrap() = Some(handle);
        manager
    }

    pub async fn current_fingerprint(&self) -> String {
        self.config.read().await.fingerprint()
    }

    pub fn get(&self, session_id: Uuid) -> Option<Arc<SessionMcpRuntime>> {
        self.runtimes.get(&session_id).map(|e| e.value().clone())
    }

    pub fn list(&self) -> Vec<(Uuid, Arc<SessionMcpRuntime>)> {
        self.runtimes
            .iter()
            .map(|e| (*e.key(), e.value().clone()))
            .collect()
    }

    /// Return a runtime for `session_id`, creating one if missing or if the
    /// current one no longer matches the active config fingerprint.
    pub async fn get_or_create(&self, session_id: Uuid) -> Arc<SessionMcpRuntime> {
        let current_fp = self.current_fingerprint().await;

        // Fast path: existing runtime with matching fingerprint.
        if let Some(rt) = self.get(session_id) {
            if rt.config_fingerprint() == current_fp && !rt.is_disposed() {
                rt.touch();
                return rt;
            }
            // Stale fingerprint or disposed: drop and rebuild.
            self.runtimes.remove(&session_id);
            rt.dispose().await;
        }

        // In-flight dedup.
        let shared = {
            let mut guard = self.in_flight.lock().await;
            if let Some(existing) = guard.get(&session_id).cloned() {
                existing
            } else {
                let future =
                    build_runtime(self.clone_arc(), session_id, current_fp.clone()).boxed();
                let shared = future.shared();
                guard.insert(session_id, shared.clone());
                shared
            }
        };

        let rt = shared.await;
        // Clean up the in-flight entry once resolved.
        self.in_flight.lock().await.remove(&session_id);
        rt
    }

    /// Drop + dispose a specific session's runtime.
    pub async fn dispose_session(&self, session_id: Uuid) {
        if let Some((_, rt)) = self.runtimes.remove(&session_id) {
            rt.dispose().await;
        }
        self.in_flight.lock().await.remove(&session_id);
    }

    /// Swap the active config. Existing runtimes keep running; the change
    /// takes effect on their next `get_or_create` visit (fingerprint diverges).
    ///
    /// Side-effect: when the new config changes `log_level` for one or
    /// more servers, live clients with matching names receive a
    /// `logging/setLevel` call. Each dispatch is fire-and-forget with a
    /// 2-second timeout so a slow server cannot stall the swap.
    pub async fn update_config(&self, new_config: McpRuntimeConfig) {
        let old_levels = {
            let guard = self.config.read().await;
            levels_map(&guard)
        };
        let new_levels = levels_map(&new_config);
        let reset_on_unset = new_config.reset_level_on_unset;
        let default_reset = new_config.default_reset_level.clone();
        {
            let mut guard = self.config.write().await;
            *guard = new_config;
        }
        let changes: Vec<(String, String)> = new_levels
            .iter()
            .filter_map(|(name, new_lvl)| {
                let old = old_levels.get(name).and_then(|o| o.as_deref());
                match (old, new_lvl.as_deref()) {
                    (o, Some(n)) if o != Some(n) => Some((name.clone(), n.to_string())),
                    (Some(_), None) if reset_on_unset => {
                        Some((name.clone(), default_reset.clone()))
                    }
                    _ => None,
                }
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        let runtimes: Vec<Arc<SessionMcpRuntime>> =
            self.runtimes.iter().map(|e| e.value().clone()).collect();
        for rt in runtimes {
            let clients = rt.clients();
            for (srv_name, level) in &changes {
                if let Some((_, client)) = clients.iter().find(|(n, _)| n == srv_name).cloned() {
                    let level = level.clone();
                    let srv = srv_name.clone();
                    tokio::spawn(async move {
                        match tokio::time::timeout(
                            Duration::from_secs(2),
                            client.set_log_level(&level),
                        )
                        .await
                        {
                            Ok(Ok(())) => tracing::info!(
                                mcp = %srv,
                                level = %level,
                                "mcp log level updated (hot-reload)"
                            ),
                            Ok(Err(e)) => tracing::warn!(
                                mcp = %srv,
                                level = %level,
                                error = %e,
                                "mcp setLevel hot-reload failed"
                            ),
                            Err(_) => tracing::warn!(
                                mcp = %srv,
                                level = %level,
                                "mcp setLevel hot-reload timeout"
                            ),
                        }
                    });
                }
            }
        }
    }

    /// Cancel the reap task and dispose every live runtime. Idempotent.
    pub async fn shutdown_all(&self) {
        self.shutdown_all_with_reason("client shutdown").await
    }

    /// Like `shutdown_all` but propagates a custom `reason` down to every
    /// live session runtime and, in turn, every live client's
    /// `notifications/cancelled` payload.
    pub async fn shutdown_all_with_reason(&self, reason: &str) {
        self.shutdown.cancel();
        if let Some(h) = self.reap_handle.lock().unwrap().take() {
            h.abort();
        }
        let ids: Vec<Uuid> = self.runtimes.iter().map(|e| *e.key()).collect();
        for id in ids {
            if let Some((_, rt)) = self.runtimes.remove(&id) {
                rt.dispose_with_reason(reason).await;
            }
        }
        self.in_flight.lock().await.clear();
    }

    // Obtain an `Arc<Self>` from `&self`. We hold a `dashmap::Arc`-like
    // handle via `Arc::clone` indirectly by having callers hold the manager
    // as `Arc`; but for internal builder we just need to clone field Arcs.
    fn clone_arc(&self) -> ManagerHandle {
        ManagerHandle {
            config: self.config.clone(),
            runtimes: self.runtimes.clone(),
        }
    }
}

/// Minimal handle passed to the build future so it can install the runtime
/// without holding a full `Arc<McpRuntimeManager>` (which would complicate
/// the `Shared<BoxFuture<'static, _>>` lifetime).
struct ManagerHandle {
    config: Arc<RwLock<McpRuntimeConfig>>,
    runtimes: Arc<DashMap<Uuid, Arc<SessionMcpRuntime>>>,
}

async fn build_runtime(
    handle: ManagerHandle,
    session_id: Uuid,
    expected_fp: String,
) -> Arc<SessionMcpRuntime> {
    // Read servers under the current config snapshot. If config shifts
    // while we're mid-build, we still honour the snapshot we captured.
    let (servers, cache_cfg, uri_allowlist) = {
        let cfg = handle.config.read().await;
        // If fingerprint has already moved on, we still build with the
        // current snapshot; the caller will re-visit next time and rebuild.
        (
            cfg.servers.clone(),
            cfg.resource_cache,
            cfg.resource_uri_allowlist.clone(),
        )
    };
    let clients = connect_all(&servers).await;
    let cache = Arc::new(crate::resource_cache::ResourceCache::new(cache_cfg));
    let rt = Arc::new(
        SessionMcpRuntime::new(session_id, expected_fp, clients)
            .with_resource_cache(cache)
            .with_resource_uri_allowlist(Arc::new(uri_allowlist)),
    );
    handle.runtimes.insert(session_id, rt.clone());
    crate::telemetry::inc_sessions_created();
    crate::telemetry::inc_sessions_active();
    rt
}

async fn reap_loop(
    manager: Arc<McpRuntimeManager>,
    interval: Duration,
    ttl: Duration,
    shutdown: CancellationToken,
) {
    let tick = interval.max(Duration::from_millis(50));
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = ticker.tick() => {
                let now = chrono::Utc::now();
                let stale: Vec<Uuid> = manager
                    .runtimes
                    .iter()
                    .filter_map(|e| {
                        let age = now.signed_duration_since(e.value().last_used_at());
                        if age.to_std().map(|d| d > ttl).unwrap_or(false) {
                            Some(*e.key())
                        } else {
                            None
                        }
                    })
                    .collect();
                for id in stale {
                    if let Some((_, rt)) = manager.runtimes.remove(&id) {
                        tracing::info!(session = %id, "mcp session idle-reaped");
                        rt.dispose_with_reason("reap").await;
                    }
                }
            }
        }
    }
}

fn levels_map(cfg: &McpRuntimeConfig) -> HashMap<String, Option<String>> {
    cfg.servers
        .iter()
        .map(|s| match s {
            McpServerRuntimeConfig::Stdio(c) => (c.name.clone(), c.log_level.clone()),
            McpServerRuntimeConfig::Http {
                name, log_level, ..
            } => (name.clone(), log_level.clone()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_cfg() -> McpRuntimeConfig {
        McpRuntimeConfig {
            servers: Vec::new(),
            session_ttl: Duration::from_millis(50),
            idle_reap_interval: Duration::from_millis(20),
            reset_level_on_unset: false,
            default_reset_level: "info".into(),
            resource_cache: Default::default(),
            resource_uri_allowlist: Vec::new(),
        }
    }

    #[test]
    fn levels_map_extracts_per_server_levels() {
        use crate::config::McpServerConfig;
        let cfg = McpRuntimeConfig {
            servers: vec![
                McpServerRuntimeConfig::Stdio(McpServerConfig {
                    name: "a".into(),
                    log_level: Some("warning".into()),
                    ..Default::default()
                }),
                McpServerRuntimeConfig::Stdio(McpServerConfig {
                    name: "b".into(),
                    log_level: None,
                    ..Default::default()
                }),
            ],
            session_ttl: Duration::from_secs(60),
            idle_reap_interval: Duration::from_secs(1),
            reset_level_on_unset: false,
            default_reset_level: "info".into(),
            resource_cache: Default::default(),
            resource_uri_allowlist: Vec::new(),
        };
        let m = levels_map(&cfg);
        assert_eq!(m.get("a").cloned().flatten().as_deref(), Some("warning"));
        assert_eq!(m.get("b").cloned().flatten(), None);
    }

    #[test]
    fn levels_map_diffs_include_reset_when_flag_on() {
        use crate::config::McpServerConfig;
        let old_cfg = McpRuntimeConfig {
            servers: vec![McpServerRuntimeConfig::Stdio(McpServerConfig {
                name: "a".into(),
                log_level: Some("warning".into()),
                ..Default::default()
            })],
            session_ttl: Duration::from_secs(60),
            idle_reap_interval: Duration::from_secs(1),
            reset_level_on_unset: false,
            default_reset_level: "info".into(),
            resource_cache: Default::default(),
            resource_uri_allowlist: Vec::new(),
        };
        let new_cfg = McpRuntimeConfig {
            servers: vec![McpServerRuntimeConfig::Stdio(McpServerConfig {
                name: "a".into(),
                log_level: None,
                ..Default::default()
            })],
            session_ttl: Duration::from_secs(60),
            idle_reap_interval: Duration::from_secs(1),
            reset_level_on_unset: true,
            default_reset_level: "info".into(),
            resource_cache: Default::default(),
            resource_uri_allowlist: Vec::new(),
        };
        let old = levels_map(&old_cfg);
        let new = levels_map(&new_cfg);
        let reset_on_unset = new_cfg.reset_level_on_unset;
        let changes: Vec<(String, String)> = new
            .iter()
            .filter_map(|(name, new_lvl)| {
                let o = old.get(name).and_then(|o| o.as_deref());
                match (o, new_lvl.as_deref()) {
                    (orig, Some(n)) if orig != Some(n) => Some((name.clone(), n.to_string())),
                    (Some(_), None) if reset_on_unset => Some((name.clone(), "info".into())),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(changes, vec![("a".to_string(), "info".to_string())]);
    }

    #[tokio::test]
    async fn update_config_noop_when_no_log_level_changes() {
        // Empty config in both — no clients, no wire calls. Exercise code
        // path to confirm it doesn't panic.
        let mgr = McpRuntimeManager::new(empty_cfg());
        mgr.update_config(empty_cfg()).await;
        mgr.shutdown_all().await;
    }

    #[tokio::test]
    async fn get_or_create_returns_same_runtime_when_fingerprint_stable() {
        let mgr = McpRuntimeManager::new(empty_cfg());
        let sid = Uuid::new_v4();
        let a = mgr.get_or_create(sid).await;
        let b = mgr.get_or_create(sid).await;
        assert!(Arc::ptr_eq(&a, &b));
        mgr.shutdown_all().await;
    }

    #[tokio::test]
    async fn dispose_session_drops_runtime() {
        let mgr = McpRuntimeManager::new(empty_cfg());
        let sid = Uuid::new_v4();
        let rt = mgr.get_or_create(sid).await;
        mgr.dispose_session(sid).await;
        assert!(rt.is_disposed());
        assert!(mgr.get(sid).is_none());
        mgr.shutdown_all().await;
    }

    #[tokio::test]
    async fn update_config_lazy_invalidation_on_fingerprint_change() {
        let cfg = empty_cfg();
        let mgr = McpRuntimeManager::new(cfg);
        let sid = Uuid::new_v4();
        let a = mgr.get_or_create(sid).await;
        let a_fp = a.config_fingerprint().to_string();

        // Install a config with a different fingerprint (add a phantom server).
        let new_cfg = McpRuntimeConfig {
            servers: vec![crate::runtime_config::McpServerRuntimeConfig::Stdio(
                crate::config::McpServerConfig {
                    name: "ghost".into(),
                    command: "/nonexistent-bin-for-fingerprint".into(),
                    ..Default::default()
                },
            )],
            ..empty_cfg()
        };
        assert_ne!(new_cfg.fingerprint(), a_fp);
        mgr.update_config(new_cfg).await;

        let b = mgr.get_or_create(sid).await;
        assert!(a.is_disposed(), "old runtime should be disposed");
        assert_ne!(b.config_fingerprint(), a_fp);
        mgr.shutdown_all().await;
    }

    #[tokio::test]
    async fn reap_evicts_idle_session() {
        let mgr = McpRuntimeManager::new(empty_cfg()); // ttl=50ms, reap=20ms
        let sid = Uuid::new_v4();
        let _ = mgr.get_or_create(sid).await;
        assert!(mgr.get(sid).is_some());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(mgr.get(sid).is_none(), "session should have been reaped");
        mgr.shutdown_all().await;
    }
}
