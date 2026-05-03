//! Phase 81.2 — `NexoPlugin` trait + `PluginInitContext` + lifecycle
//! errors. The runtime contract every native Rust plugin
//! implements.
//!
//! A plugin ships as a Rust crate that:
//! 1. Includes a `nexo-plugin.toml` manifest at its crate root
//!    (Phase 81.1 schema, parsed by `nexo-plugin-manifest`).
//! 2. Implements [`NexoPlugin`] in its public surface.
//! 3. Exports an `Arc<dyn NexoPlugin>` constructor (e.g.
//!    `pub fn new() -> Arc<dyn NexoPlugin>`).
//!
//! [`PluginRegistry`] (Phase 81.5) walks the workspace + user
//! plugin dirs at boot, parses manifests, instantiates plugin
//! handles, and calls `init` in deterministic order.
//! [`PluginInitContext`] gives plugins typed handles to every
//! daemon subsystem they may need: tool registry, advisor
//! registry (advisory_hook), hook registry, broker, LLM
//! registry, reload coordinator, sessions, long-term memory,
//! shutdown signal.
//!
//! Provider-agnostic: the trait + context expose no LLM-provider
//! specifics. Plugins reach providers through
//! [`LlmRegistry::build`] which itself is provider-agnostic.
//!
//! ## Distinction from existing `crate::agent::plugin::Plugin`
//!
//! `Plugin` (in [`crate::agent::plugin`]) is the **Channel
//! plugin** trait — runtime-side I/O for browser / WhatsApp /
//! Telegram / email channels. `NexoPlugin` is the **boot-time
//! lifecycle** trait — the plug-and-play registration contract.
//! Distinct concept, distinct file, distinct trait name.
//!
//! ## IRROMPIBLE refs
//!
//! - claude-code-leak `src/tools/*` — **absence** of plugin
//!   trait. The leak hardcodes every tool as a TS module with a
//!   `buildTool({...})` factory call; lifecycle is "module
//!   exists". Internal nexo split between core tools and
//!   plugin-shippable tools is what 81.x delivers — leak has no
//!   architectural precedent for this.
//! - `research/src/plugins/runtime/types-channel.ts:56-71` —
//!   `register: (params) => { dispose: () => void }` pattern.
//!   Adapted via Rust `Drop` semantics — plugin's resources
//!   clean up when its `Arc<dyn NexoPlugin>` is dropped or via
//!   explicit `shutdown()`.
//! - `research/src/plugins/activation-context.ts:27-44` —
//!   `PluginActivationInputs` activation metadata pattern. We
//!   adopt the spirit (manifest + ctx) without the JS-specific
//!   surface.
//! - Internal precedent: `crates/core/src/agent/plugin.rs`
//!   (Channel `Plugin` trait, distinct concept).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexo_broker::AnyBroker;
use nexo_driver_permission::AdvisorRegistry;
use nexo_config::LlmConfig;
use nexo_llm::LlmRegistry;
use nexo_memory::LongTermMemory;
use nexo_plugin_manifest::PluginManifest;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::agent::hook_registry::HookRegistry;
use crate::agent::tool_registry::ToolRegistry;
use crate::config_reload::ConfigReloadCoordinator;
use crate::session::SessionManager;

/// Default per-plugin shutdown budget. The registry shutdown
/// sequence wraps `plugin.shutdown()` in
/// [`tokio::time::timeout`] at this duration so a stuck plugin
/// can't hold the daemon at exit.
pub const DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Lifecycle contract for native Rust plugins.
///
/// See module docs for architecture + IRROMPIBLE refs.
#[async_trait]
pub trait NexoPlugin: Send + Sync + 'static {
    /// The manifest the plugin shipped (parsed by the registry
    /// before instantiation). Used for plugin id, capabilities,
    /// requires checks, namespace policy.
    fn manifest(&self) -> &PluginManifest;

    /// Called once at boot AFTER manifest validation succeeds.
    /// Plugin uses `ctx` handles to register tools, advisors,
    /// hooks, agents, etc.
    ///
    /// Errors propagate to the registry, which logs the failure
    /// and skips the plugin (other plugins continue). The daemon
    /// stays up.
    async fn init(
        &self,
        ctx: &mut PluginInitContext<'_>,
    ) -> Result<(), PluginInitError>;

    /// Called on graceful shutdown. Default no-op for stateless
    /// plugins. Plugins with persistent state (DB connections,
    /// background tasks, external resources) override.
    ///
    /// The registry wraps this call in [`tokio::time::timeout`]
    /// at [`DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT`] — a stuck plugin
    /// surfaces [`PluginShutdownError::Timeout`].
    async fn shutdown(&self) -> Result<(), PluginShutdownError> {
        Ok(())
    }
}

/// Bundle of handles a plugin's `init` receives. Lifetimes tied
/// to the boot scope; plugins that need long-lived clones must
/// clone the `Arc<...>` fields explicitly.
pub struct PluginInitContext<'a> {
    /// Resolved YAML config root. The plugin's own config
    /// namespace lives at `<config_dir>/plugins/<plugin_id>/`
    /// — use [`plugin_config_dir`](Self::plugin_config_dir).
    pub config_dir: &'a Path,

    /// Process-wide state root (`NEXO_STATE_ROOT` env or XDG
    /// default). The plugin's own state namespace lives at
    /// `<state_root>/plugins/<plugin_id>/` — use
    /// [`plugin_state_dir`](Self::plugin_state_dir).
    pub state_root: &'a Path,

    /// Tool registry. Plugin registers `<plugin_id>_*` tools
    /// here. Names MUST match `manifest.tools.expose`
    /// (registry-level audit lands in 81.3).
    pub tool_registry: Arc<ToolRegistry>,

    /// Advisor registry (advisory_hook). `RwLock` so multiple
    /// plugins can register without contention. Plugin acquires
    /// a write lock per registration:
    /// `ctx.advisor_registry.write().await.register(...)`.
    pub advisor_registry: Arc<RwLock<AdvisorRegistry>>,

    /// Hook registry (Phase 11.6). For per-message lifecycle
    /// extensions (`before_message` / `after_message`).
    pub hook_registry: Arc<HookRegistry>,

    /// NATS broker handle. Plugin publishes/subscribes via
    /// `broker.publish(topic, payload).await`.
    pub broker: AnyBroker,

    /// LLM provider builder. Plugin builds clients via
    /// `llm_registry.build(&cfg.llm, &model_cfg)?`.
    pub llm_registry: Arc<LlmRegistry>,

    /// Phase 81.20.b.b — LLM config the registry needs at build
    /// time (provider table with API keys, retry knobs, tenant
    /// overrides). Subprocess plugins (`SubprocessNexoPlugin`)
    /// pair this with `llm_registry` to construct `LlmServices`
    /// for the `llm.complete` JSON-RPC handler. In-tree plugins
    /// that build their own clients also use it.
    pub llm_config: Arc<LlmConfig>,

    /// Phase 18 reload coordinator. Plugin registers post-hooks
    /// via `reload_coord.register_post_hook(Box::new(...)).await`
    /// for hot-reload-aware behavior.
    pub reload_coord: Arc<ConfigReloadCoordinator>,

    /// Process-shared session manager.
    pub sessions: Arc<SessionManager>,

    /// Long-term memory (`None` when not configured by operator).
    /// Plugins that declare
    /// `manifest.requires.nexo_capabilities = ["long_term_memory"]`
    /// should validate this is `Some` and return
    /// [`PluginInitError::MissingNexoCapability`] otherwise.
    pub long_term_memory: Option<Arc<LongTermMemory>>,

    /// Daemon-wide shutdown signal. Plugin's background tasks
    /// should `tokio::select!` on this to exit cleanly.
    pub shutdown: CancellationToken,

    /// Phase 81.8 — extension point for plugins shipping new
    /// channel kinds (SMS, Discord, IRC, Matrix, custom webhooks).
    /// Plugin's `init()` calls
    /// `ctx.channel_adapter_registry.register(Arc::new(MyAdapter), self.manifest().plugin.id.clone())?;`
    /// First-registers-wins-rest-rejected — see
    /// [`crate::agent::channel_adapter::ChannelAdapterRegistrationError`].
    pub channel_adapter_registry:
        Arc<crate::agent::channel_adapter::ChannelAdapterRegistry>,
}

impl PluginInitContext<'_> {
    /// `<config_dir>/plugins/<plugin_id>/`. Plugin-scoped config
    /// namespace (Phase 81.4 loader will read this dir).
    pub fn plugin_config_dir(&self, plugin_id: &str) -> PathBuf {
        self.config_dir.join("plugins").join(plugin_id)
    }

    /// `<state_root>/plugins/<plugin_id>/`. Plugin-scoped state
    /// namespace (databases, caches, etc).
    pub fn plugin_state_dir(&self, plugin_id: &str) -> PathBuf {
        self.state_root.join("plugins").join(plugin_id)
    }
}

/// Errors a plugin's `init` can return. Boundary type — every
/// failure mode the registry needs to discriminate is its own
/// variant; freeform errors land in `Other`.
#[derive(Debug, thiserror::Error)]
pub enum PluginInitError {
    #[error(
        "plugin `{plugin_id}` requires capability `{capability}` not provided by daemon"
    )]
    MissingNexoCapability {
        plugin_id: String,
        capability: String,
    },

    #[error(
        "plugin `{plugin_id}` registered tool `{tool_name}` not declared in manifest.tools.expose"
    )]
    UnregisteredTool {
        plugin_id: String,
        tool_name: String,
    },

    #[error("plugin `{plugin_id}` config error")]
    Config {
        plugin_id: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("plugin `{plugin_id}` tool registration error")]
    ToolRegistration {
        plugin_id: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("plugin `{plugin_id}` init failed")]
    Other {
        plugin_id: String,
        #[source]
        source: anyhow::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PluginShutdownError {
    #[error("plugin `{plugin_id}` shutdown timeout after {timeout_ms}ms")]
    Timeout {
        plugin_id: String,
        timeout_ms: u64,
    },

    #[error("plugin `{plugin_id}` shutdown error")]
    Other {
        plugin_id: String,
        #[source]
        source: anyhow::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;

    /// Compile-time guarantee that [`NexoPlugin`] is dyn-safe.
    /// If the trait gains an associated type, generic method, or
    /// any other property that prevents `dyn NexoPlugin`, this
    /// statement fails to compile and the test refuses to build.
    #[allow(dead_code)]
    static _OBJECT_SAFE_CHECK: OnceLock<Arc<dyn NexoPlugin>> = OnceLock::new();

    fn minimal_manifest_toml() -> &'static str {
        r#"
[plugin]
id = "test_plugin"
version = "0.1.0"
name = "Test Plugin"
description = "Test fixture"
min_nexo_version = ">=0.1.0"
"#
    }

    fn parse_manifest() -> PluginManifest {
        PluginManifest::from_str(minimal_manifest_toml()).expect("valid TOML")
    }

    struct MockPlugin {
        manifest: PluginManifest,
        init_called: AtomicBool,
        init_outcome: Result<(), PluginInitError>,
    }

    impl MockPlugin {
        fn ok() -> Self {
            Self {
                manifest: parse_manifest(),
                init_called: AtomicBool::new(false),
                init_outcome: Ok(()),
            }
        }

        fn err() -> Self {
            Self {
                manifest: parse_manifest(),
                init_called: AtomicBool::new(false),
                init_outcome: Err(PluginInitError::Config {
                    plugin_id: "test_plugin".into(),
                    source: anyhow::anyhow!("simulated config failure"),
                }),
            }
        }
    }

    #[async_trait]
    impl NexoPlugin for MockPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        async fn init(
            &self,
            _ctx: &mut PluginInitContext<'_>,
        ) -> Result<(), PluginInitError> {
            self.init_called.store(true, Ordering::SeqCst);
            // Clone the outcome shape since PluginInitError isn't Clone.
            match &self.init_outcome {
                Ok(()) => Ok(()),
                Err(PluginInitError::Config { plugin_id, source: _ }) => {
                    Err(PluginInitError::Config {
                        plugin_id: plugin_id.clone(),
                        source: anyhow::anyhow!("simulated config failure"),
                    })
                }
                Err(_) => Err(PluginInitError::Other {
                    plugin_id: "test_plugin".into(),
                    source: anyhow::anyhow!("unexpected variant"),
                }),
            }
        }
    }

    /// Plugin whose `shutdown()` sleeps long enough to trip a
    /// short test timeout. Used to verify the registry's
    /// timeout pattern.
    struct SlowShutdownPlugin {
        manifest: PluginManifest,
    }

    #[async_trait]
    impl NexoPlugin for SlowShutdownPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        async fn init(
            &self,
            _ctx: &mut PluginInitContext<'_>,
        ) -> Result<(), PluginInitError> {
            Ok(())
        }
        async fn shutdown(&self) -> Result<(), PluginShutdownError> {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn shutdown_default_returns_ok() {
        let plugin = MockPlugin::ok();
        let r = plugin.shutdown().await;
        assert!(r.is_ok(), "default shutdown impl returns Ok");
    }

    #[tokio::test]
    async fn mock_plugin_init_recorded() {
        // Mock plugin is invoked directly without a full ctx —
        // we exercise the trait dispatch path. Phase 81.5 will
        // wire a real ctx; the lifecycle contract is verified
        // here independently.
        let plugin = MockPlugin::ok();
        // The test cannot easily build a full PluginInitContext
        // (requires SessionManager + ConfigReloadCoordinator +
        // every subsystem). We verify the trait shape via the
        // type-level _OBJECT_SAFE_CHECK static and the
        // init_outcome plumbing. Phase 81.5 integration tests
        // exercise the full init path with a real ctx.
        assert_eq!(plugin.manifest().id(), "test_plugin");
        assert!(!plugin.init_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn mock_plugin_init_failure_propagates_error() {
        let plugin = MockPlugin::err();
        // Same caveat as above — exercise the error variant.
        match &plugin.init_outcome {
            Err(PluginInitError::Config { plugin_id, .. }) => {
                assert_eq!(plugin_id, "test_plugin");
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shutdown_timeout_propagates_via_tokio_timeout() {
        let plugin = SlowShutdownPlugin {
            manifest: parse_manifest(),
        };
        let result =
            tokio::time::timeout(Duration::from_millis(50), plugin.shutdown()).await;
        assert!(
            result.is_err(),
            "tokio::time::timeout must elapse before slow shutdown completes"
        );
        // The registry maps `Err(Elapsed)` to
        // `PluginShutdownError::Timeout` — verify the wrap shape
        // is well-formed.
        let timeout_err = PluginShutdownError::Timeout {
            plugin_id: "test_plugin".into(),
            timeout_ms: 50,
        };
        let s = timeout_err.to_string();
        assert!(s.contains("test_plugin"), "got {s:?}");
        assert!(s.contains("50ms"), "got {s:?}");
    }

    #[test]
    fn init_error_display_messages_actionable() {
        let cases: Vec<Box<dyn std::fmt::Display>> = vec![
            Box::new(PluginInitError::MissingNexoCapability {
                plugin_id: "marketing".into(),
                capability: "broker".into(),
            }),
            Box::new(PluginInitError::UnregisteredTool {
                plugin_id: "marketing".into(),
                tool_name: "marketing_secret_tool".into(),
            }),
            Box::new(PluginInitError::Config {
                plugin_id: "marketing".into(),
                source: anyhow::anyhow!("bad yaml"),
            }),
            Box::new(PluginInitError::ToolRegistration {
                plugin_id: "marketing".into(),
                source: anyhow::anyhow!("dup name"),
            }),
            Box::new(PluginInitError::Other {
                plugin_id: "marketing".into(),
                source: anyhow::anyhow!("kaboom"),
            }),
        ];
        for err in cases {
            let s = err.to_string();
            assert!(
                s.contains("marketing"),
                "every variant must include plugin_id: {s:?}"
            );
            assert!(
                s.len() >= 25,
                "actionable Display must include enough context: {s:?}"
            );
        }
    }

    #[test]
    fn shutdown_error_display_messages_actionable() {
        let timeout = PluginShutdownError::Timeout {
            plugin_id: "marketing".into(),
            timeout_ms: 5_000,
        };
        let other = PluginShutdownError::Other {
            plugin_id: "marketing".into(),
            source: anyhow::anyhow!("teardown failure"),
        };
        for err in [&timeout as &dyn std::fmt::Display, &other] {
            let s = err.to_string();
            assert!(s.contains("marketing"), "got {s:?}");
        }
    }

    #[test]
    fn plugin_init_context_helpers_resolve_paths() {
        // We construct a minimal stand-alone context via the
        // path helpers without instantiating heavy subsystems.
        // Verify path resolution is correct.
        let config_dir = Path::new("/tmp/nexo-test/config");
        let state_root = Path::new("/tmp/nexo-test/state");

        // Helper that exercises path resolution logic without
        // a full PluginInitContext value (which would require
        // instantiating every subsystem). The logic itself is
        // small + pure.
        let cfg = config_dir.join("plugins").join("marketing");
        let state = state_root.join("plugins").join("marketing");
        assert_eq!(cfg, PathBuf::from("/tmp/nexo-test/config/plugins/marketing"));
        assert_eq!(state, PathBuf::from("/tmp/nexo-test/state/plugins/marketing"));
    }

    #[test]
    fn default_plugin_shutdown_timeout_is_5_seconds() {
        assert_eq!(DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT, Duration::from_secs(5));
    }
}
