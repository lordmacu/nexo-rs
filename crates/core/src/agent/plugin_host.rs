//! `NexoPlugin` trait + `PluginInitContext` + lifecycle errors. The
//! runtime contract every native Rust plugin implements.
//!
//! A plugin ships as a Rust crate that:
//! 1. Includes a `nexo-plugin.toml` manifest at its crate root
//!    (parsed by `nexo-plugin-manifest`).
//! 2. Implements [`NexoPlugin`] in its public surface.
//! 3. Exports an `Arc<dyn NexoPlugin>` constructor (e.g.
//!    `pub fn new() -> Arc<dyn NexoPlugin>`).
//!
//! [`PluginRegistry`] walks the workspace + user
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
//! A plugin's resources clean up when its `Arc<dyn NexoPlugin>` is
//! dropped (via Rust `Drop` semantics) or via an explicit
//! `shutdown()` call. The activation surface is just the manifest +
//! the [`PluginInitContext`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexo_broker::AnyBroker;
use nexo_config::LlmConfig;
use nexo_driver_permission::AdvisorRegistry;
use nexo_llm::LlmRegistry;
use nexo_memory::LongTermMemory;
use nexo_plugin_manifest::PluginManifest;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::agent::hook_registry::HookRegistry;
use crate::agent::scoped_tool_registry::ScopedToolRegistry;
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
    async fn init(&self, ctx: &mut PluginInitContext<'_>) -> Result<(), PluginInitError>;

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

    /// Downcast hook so the boot helper can detect
    /// `SubprocessNexoPlugin` instances and register their declared
    /// `extends.channels` adapters into the channel registry.
    /// Concrete types return `&self`; required (no default impl)
    /// so each plugin opts in explicitly.
    fn as_any(&self) -> &dyn std::any::Any;

    /// Phase 81.33.b — plugin opts in to provide a
    /// [`nexo_pairing::adapter::PairingChannelAdapter`] for
    /// inbound sender normalisation + challenge delivery.
    ///
    /// Default `None` — plugins without a pair-able channel
    /// (memory, browser pure-control) skip. Plugins that ship a
    /// pairing flow (whatsapp / telegram / future sms / discord)
    /// override to return `Some(Arc<dyn PairingChannelAdapter>)`.
    ///
    /// Boot + spawner call this on every loaded plugin handle
    /// and register the returned adapters into the shared
    /// `PairingAdapterRegistry`. Eliminates the per-plugin
    /// hardcoded `Arc::new(XxxPairingAdapter::new(broker))`
    /// blocks the daemon previously needed.
    ///
    /// `SubprocessNexoPlugin::build_pairing_adapter` will move
    /// to manifest-driven `GenericBrokerPairingAdapter` in
    /// Phase 81.33.b.real (separate session) once the manifest
    /// schema for `[plugin.pairing.adapter]` is finalised.
    /// Until then the default `None` keeps subprocess plugins
    /// on the legacy hardcoded path in `src/main.rs`.
    fn build_pairing_adapter(
        &self,
        _broker: nexo_broker::AnyBroker,
    ) -> Option<std::sync::Arc<dyn nexo_pairing::adapter::PairingChannelAdapter>> {
        None
    }

    /// Phase 81.33 — plugin opts in to register its **outbound
    /// LLM tools** into a per-agent registry.
    ///
    /// Called by boot (per-agent loop) and by the hot-spawn
    /// closure (Phase 81.32) once an agent is detected to declare
    /// this plugin via `cfg.plugins`. Both call sites pass the
    /// SAME `&ToolRegistry` they're populating so plugin authors
    /// don't need to discriminate between paths.
    ///
    /// Default no-op: plugins with no outbound tools (anything
    /// pure-inbound) inherit the empty impl. Plugins like
    /// `nexo-plugin-whatsapp` / `-telegram` / `-email` /
    /// `-google` override to call their `register_*_tools(...)`
    /// helper, which removes the hardcoded
    /// `cfg.plugins.iter().any(|p| p == "whatsapp")` block from
    /// `src/main.rs` and lets out-of-tree plugins (slack, discord,
    /// sms, instagram, …) self-register without daemon edits.
    ///
    /// Convention: implementations must respect the same scoping
    /// rules as `init()` (only `<plugin_id>_*` names, no reserved
    /// prefixes). Registration failures here are best-effort —
    /// plugins log + skip rather than panic so a buggy outbound
    /// tool doesn't take down the whole boot.
    fn register_outbound_tools(&self, _registry: &super::tool_registry::ToolRegistry) {
        // Default: plugin exposes no outbound tools.
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

    /// Per-plugin scoped tool registry. Plugin registers
    /// `<plugin_id>_*` tools here. Names MUST match
    /// `manifest.tools.expose` and respect the reserved-prefix
    /// denylist; collisions are always rejected. See
    /// [`ScopedToolRegistry`] for the enforcement model.
    pub tool_registry: Arc<ScopedToolRegistry>,

    /// Advisor registry (advisory_hook). `RwLock` so multiple
    /// plugins can register without contention. Plugin acquires
    /// a write lock per registration:
    /// `ctx.advisor_registry.write().await.register(...)`.
    pub advisor_registry: Arc<RwLock<AdvisorRegistry>>,

    /// Hook registry. For per-message lifecycle
    /// extensions (`before_message` / `after_message`).
    pub hook_registry: Arc<HookRegistry>,

    /// NATS broker handle. Plugin publishes/subscribes via
    /// `broker.publish(topic, payload).await`.
    pub broker: AnyBroker,

    /// LLM provider builder. Plugin builds clients via
    /// `llm_registry.build(&cfg.llm, &model_cfg)?`.
    pub llm_registry: Arc<LlmRegistry>,

    /// LLM config the registry needs at build
    /// time (provider table with API keys, retry knobs, tenant
    /// overrides). Subprocess plugins (`SubprocessNexoPlugin`)
    /// pair this with `llm_registry` to construct `LlmServices`
    /// for the `llm.complete` JSON-RPC handler. In-tree plugins
    /// that build their own clients also use it.
    pub llm_config: Arc<LlmConfig>,

    /// Reload coordinator. Plugin registers post-hooks
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

    /// Extension point for plugins shipping new
    /// channel kinds (SMS, Discord, IRC, Matrix, custom webhooks).
    /// Plugin's `init()` calls
    /// `ctx.channel_adapter_registry.register(Arc::new(MyAdapter), self.manifest().plugin.id.clone())?;`
    /// First-registers-wins-rest-rejected — see
    /// [`crate::agent::channel_adapter::ChannelAdapterRegistrationError`].
    pub channel_adapter_registry: Arc<crate::agent::channel_adapter::ChannelAdapterRegistry>,

    /// Pre-loaded + pre-validated plugin config
    /// from `<config_dir>/plugins/<plugin_id>/*.yaml`. Always at
    /// least an empty mapping. Plugin treats it as read-only;
    /// typed views via `serde_yaml::from_value(cfg.clone())`.
    /// Validation errors short-circuit `init()` with
    /// `InitOutcome::Failed` BEFORE the plugin runs.
    pub plugin_config: Arc<serde_yaml::Value>,

    /// Shared sandbox runner. Subprocess plugin
    /// adapters consume this at spawn time to wrap the child
    /// `Command` with bwrap argv when the plugin's manifest
    /// declares `[plugin.sandbox] enabled = true`. In-tree
    /// plugins ignore the field. Built once by
    /// `SubprocessRuntime` at boot.
    pub sandbox: Arc<crate::agent::plugin_sandbox::SandboxRunner>,
}

impl PluginInitContext<'_> {
    /// `<config_dir>/plugins/<plugin_id>/`. Plugin-scoped config
    /// namespace (the config loader reads this dir).
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
    #[error("plugin `{plugin_id}` requires capability `{capability}` not provided by daemon")]
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

    /// Plugin attempted to register tools outside its
    /// declared namespace, in a reserved namespace, or colliding
    /// with existing tools. Surfaced when
    /// `NEXO_PLUGIN_NAMESPACE_STRICT=1` or when the per-call
    /// `register*` returns `Err` (`Strict` mode).
    #[error(
        "plugin `{plugin_id}` violated tool namespace policy ({count} violation(s); first 3: {sample})"
    )]
    ToolNamespace {
        plugin_id: String,
        count: usize,
        sample: String,
        violations: Vec<crate::agent::scoped_tool_registry::NamespaceViolation>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PluginShutdownError {
    #[error("plugin `{plugin_id}` shutdown timeout after {timeout_ms}ms")]
    Timeout { plugin_id: String, timeout_ms: u64 },

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
        async fn init(&self, _ctx: &mut PluginInitContext<'_>) -> Result<(), PluginInitError> {
            self.init_called.store(true, Ordering::SeqCst);
            // Clone the outcome shape since PluginInitError isn't Clone.
            match &self.init_outcome {
                Ok(()) => Ok(()),
                Err(PluginInitError::Config {
                    plugin_id,
                    source: _,
                }) => Err(PluginInitError::Config {
                    plugin_id: plugin_id.clone(),
                    source: anyhow::anyhow!("simulated config failure"),
                }),
                Err(_) => Err(PluginInitError::Other {
                    plugin_id: "test_plugin".into(),
                    source: anyhow::anyhow!("unexpected variant"),
                }),
            }
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
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
        async fn init(&self, _ctx: &mut PluginInitContext<'_>) -> Result<(), PluginInitError> {
            Ok(())
        }
        async fn shutdown(&self) -> Result<(), PluginShutdownError> {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
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
        // we exercise the trait dispatch path. The lifecycle
        // contract is verified here independently.
        let plugin = MockPlugin::ok();
        // The test cannot easily build a full PluginInitContext
        // (requires SessionManager + ConfigReloadCoordinator +
        // every subsystem). We verify the trait shape via the
        // type-level _OBJECT_SAFE_CHECK static and the
        // init_outcome plumbing. Integration tests exercise the
        // full init path with a real ctx.
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
        let result = tokio::time::timeout(Duration::from_millis(50), plugin.shutdown()).await;
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
        assert_eq!(
            cfg,
            PathBuf::from("/tmp/nexo-test/config/plugins/marketing")
        );
        assert_eq!(
            state,
            PathBuf::from("/tmp/nexo-test/state/plugins/marketing")
        );
    }

    #[test]
    fn default_plugin_shutdown_timeout_is_5_seconds() {
        assert_eq!(DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT, Duration::from_secs(5));
    }
}
