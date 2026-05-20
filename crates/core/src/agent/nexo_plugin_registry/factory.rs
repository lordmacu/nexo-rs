//! Manifest-driven plugin factory infrastructure.
//!
//! `PluginFactoryRegistry` maps `plugin.id` → closure that builds
//! a concrete `Arc<dyn NexoPlugin>` from the parsed manifest.
//! Callers populate this registry at boot;
//! `run_plugin_init_loop_with_factory` then consumes it to
//! instantiate + initialize each plugin.

use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

use nexo_plugin_manifest::PluginManifest;

use crate::agent::plugin_host::NexoPlugin;

/// Type-erased error returned by plugin factory
/// closures. Each plugin author can use their own error type;
/// the registry boxes them so storage stays homogeneous.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Closure that constructs an `Arc<dyn NexoPlugin>`
/// from a parsed manifest. The closure typically captures
/// references to the operator's `&AppConfig` (or specific config
/// slices) at registration time so it can build plugin-specific
/// config structs from the running daemon's state.
pub type PluginFactory =
    Box<dyn Fn(&PluginManifest) -> Result<Arc<dyn NexoPlugin>, BoxError> + Send + Sync + 'static>;

/// Factory map keyed by `plugin.id`. Callers populate this at boot;
/// the `run_plugin_init_loop_with_factory` helper consults it to
/// instantiate concrete plugins from discovered manifests.
#[derive(Default)]
pub struct PluginFactoryRegistry {
    // Interior-mutable so the registry can be shared as `Arc` AND still
    // gain/drop factories at runtime (multi-instance hot-registration:
    // a channel instance configured after boot needs its own factory
    // registered live). Boot still populates it via `register`.
    factories: std::sync::RwLock<BTreeMap<String, PluginFactory>>,
}

impl std::fmt::Debug for PluginFactoryRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.factories.read().unwrap_or_else(|p| p.into_inner());
        f.debug_struct("PluginFactoryRegistry")
            .field("kinds", &guard.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl PluginFactoryRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a factory under `plugin_id`. First-registers-wins:
    /// duplicate registration returns `Err(AlreadyRegistered)` and
    /// the prior factory stays in place. Interior-mutable (`&self`) so
    /// it works after the registry is wrapped in `Arc`.
    pub fn register(
        &self,
        plugin_id: impl Into<String>,
        factory: PluginFactory,
    ) -> Result<(), FactoryRegistrationError> {
        let id = plugin_id.into();
        let mut guard = self.factories.write().unwrap_or_else(|p| p.into_inner());
        if guard.contains_key(&id) {
            return Err(FactoryRegistrationError::AlreadyRegistered { plugin_id: id });
        }
        guard.insert(id, factory);
        Ok(())
    }

    /// Runtime (re)registration — overwrite-allowed so a reconfigured
    /// or renamed channel instance replaces its prior factory. Returns
    /// whether a prior factory was replaced. Distinct from [`register`]
    /// which is first-wins for the boot path.
    pub fn register_runtime(&self, plugin_id: impl Into<String>, factory: PluginFactory) -> bool {
        let id = plugin_id.into();
        let mut guard = self.factories.write().unwrap_or_else(|p| p.into_inner());
        guard.insert(id, factory).is_some()
    }

    /// Drop a runtime-registered factory (an uninstalled / removed
    /// instance). Returns whether one existed.
    pub fn unregister_runtime(&self, plugin_id: &str) -> bool {
        let mut guard = self.factories.write().unwrap_or_else(|p| p.into_inner());
        guard.remove(plugin_id).is_some()
    }

    /// Instantiate the plugin registered under `plugin_id` by
    /// calling its factory closure with the parsed `manifest`.
    pub fn instantiate(
        &self,
        plugin_id: &str,
        manifest: &PluginManifest,
    ) -> Result<Arc<dyn NexoPlugin>, FactoryInstantiateError> {
        let guard = self.factories.read().unwrap_or_else(|p| p.into_inner());
        let factory =
            guard
                .get(plugin_id)
                .ok_or_else(|| FactoryInstantiateError::NotRegistered {
                    plugin_id: plugin_id.to_string(),
                })?;
        factory(manifest).map_err(|source| FactoryInstantiateError::FactoryFailed {
            plugin_id: plugin_id.to_string(),
            source,
        })
    }

    pub fn is_registered(&self, plugin_id: &str) -> bool {
        self.factories
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(plugin_id)
    }

    /// Sorted list of registered plugin ids. Stable iteration for
    /// observability + tests.
    pub fn kinds(&self) -> Vec<String> {
        self.factories
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.factories
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.factories
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
    }
}

#[derive(Debug, Error)]
pub enum FactoryRegistrationError {
    #[error("plugin factory for id `{plugin_id}` already registered")]
    AlreadyRegistered { plugin_id: String },
}

#[derive(Debug, Error)]
pub enum FactoryInstantiateError {
    #[error("no plugin factory registered for id `{plugin_id}`")]
    NotRegistered { plugin_id: String },
    #[error("plugin factory for id `{plugin_id}` failed: {source}")]
    FactoryFailed {
        plugin_id: String,
        #[source]
        source: BoxError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;

    use crate::agent::plugin_host::{PluginInitContext, PluginInitError, PluginShutdownError};

    /// Mock NexoPlugin used across factory tests. Owns a manifest
    /// instance so `manifest()` can return a stable reference.
    struct MockPlugin {
        manifest: PluginManifest,
    }

    impl MockPlugin {
        fn build(plugin_id: &str) -> Self {
            let raw = format!(
                "[plugin]\n\
                 id = \"{plugin_id}\"\n\
                 version = \"0.1.0\"\n\
                 name = \"{plugin_id}\"\n\
                 description = \"factory test\"\n\
                 min_nexo_version = \">=0.0.1\"\n",
            );
            Self {
                manifest: toml::from_str(&raw).unwrap(),
            }
        }
    }

    #[async_trait]
    impl NexoPlugin for MockPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        async fn init(&self, _ctx: &mut PluginInitContext<'_>) -> Result<(), PluginInitError> {
            Ok(())
        }
        async fn shutdown(&self) -> Result<(), PluginShutdownError> {
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn fixture_manifest(plugin_id: &str) -> PluginManifest {
        let raw = format!(
            "[plugin]\n\
             id = \"{plugin_id}\"\n\
             version = \"0.1.0\"\n\
             name = \"{plugin_id}\"\n\
             description = \"fixture\"\n\
             min_nexo_version = \">=0.0.1\"\n",
        );
        toml::from_str(&raw).unwrap()
    }

    #[test]
    fn register_first_succeeds_and_kinds_lists_it() {
        let reg = PluginFactoryRegistry::new();
        assert!(reg.is_empty());
        let factory: PluginFactory = Box::new(|_m| Ok(Arc::new(MockPlugin::build("alpha"))));
        reg.register("alpha", factory).expect("first register ok");
        assert_eq!(reg.len(), 1);
        assert!(reg.is_registered("alpha"));
        assert!(!reg.is_registered("beta"));
        assert_eq!(reg.kinds(), vec!["alpha".to_string()]);
    }

    #[test]
    fn register_duplicate_returns_already_registered() {
        let reg = PluginFactoryRegistry::new();
        let f1: PluginFactory = Box::new(|_m| Ok(Arc::new(MockPlugin::build("alpha"))));
        let f2: PluginFactory = Box::new(|_m| Ok(Arc::new(MockPlugin::build("alpha"))));
        reg.register("alpha", f1).unwrap();
        let err = reg.register("alpha", f2).expect_err("duplicate must fail");
        match err {
            FactoryRegistrationError::AlreadyRegistered { plugin_id } => {
                assert_eq!(plugin_id, "alpha");
            }
        }
        // Map size unchanged.
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn register_runtime_overwrites_and_unregister_drops() {
        let reg = PluginFactoryRegistry::new();
        let f1: PluginFactory = Box::new(|_m| Ok(Arc::new(MockPlugin::build("a"))));
        reg.register("a", f1).unwrap();
        // first-wins `register` still rejects a duplicate.
        let dup: PluginFactory = Box::new(|_m| Ok(Arc::new(MockPlugin::build("a"))));
        assert!(reg.register("a", dup).is_err());
        // `register_runtime` overwrites in place → reports replacement.
        let f2: PluginFactory = Box::new(|_m| Ok(Arc::new(MockPlugin::build("a"))));
        assert!(
            reg.register_runtime("a", f2),
            "existing id must report replaced=true"
        );
        assert_eq!(reg.len(), 1);
        // `register_runtime` of a fresh id reports no replacement.
        let fb: PluginFactory = Box::new(|_m| Ok(Arc::new(MockPlugin::build("b"))));
        assert!(
            !reg.register_runtime("b", fb),
            "fresh id must report replaced=false"
        );
        assert!(reg.is_registered("b"));
        // `unregister_runtime` drops only an existing id.
        assert!(reg.unregister_runtime("b"));
        assert!(!reg.is_registered("b"));
        assert!(!reg.unregister_runtime("ghost"));
    }

    #[test]
    fn instantiate_unregistered_returns_not_registered() {
        let reg = PluginFactoryRegistry::new();
        let m = fixture_manifest("ghost");
        match reg.instantiate("ghost", &m) {
            Err(FactoryInstantiateError::NotRegistered { plugin_id }) => {
                assert_eq!(plugin_id, "ghost");
            }
            Err(other) => panic!("expected NotRegistered, got {other:?}"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }

    #[test]
    fn instantiate_factory_error_propagates_as_factory_failed() {
        let reg = PluginFactoryRegistry::new();
        let factory: PluginFactory = Box::new(|_m| {
            let err: BoxError = Box::new(std::io::Error::other("boom"));
            Err(err)
        });
        reg.register("breaky", factory).unwrap();
        let m = fixture_manifest("breaky");
        match reg.instantiate("breaky", &m) {
            Err(FactoryInstantiateError::FactoryFailed { plugin_id, source }) => {
                assert_eq!(plugin_id, "breaky");
                assert!(format!("{source}").contains("boom"));
            }
            Err(other) => panic!("expected FactoryFailed, got {other:?}"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }

    #[test]
    fn instantiate_success_returns_arc_handle() {
        let reg = PluginFactoryRegistry::new();
        let factory: PluginFactory = Box::new(|_m| Ok(Arc::new(MockPlugin::build("alpha"))));
        reg.register("alpha", factory).unwrap();
        let m = fixture_manifest("alpha");
        match reg.instantiate("alpha", &m) {
            Ok(handle) => assert_eq!(handle.manifest().plugin.id, "alpha"),
            Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }
}
