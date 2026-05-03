//! Phase 81.12.0 — manifest-driven plugin factory infrastructure.
//!
//! `PluginFactoryRegistry` maps `plugin.id` → closure that builds
//! a concrete `Arc<dyn NexoPlugin>` from the parsed manifest.
//! Per-plugin migration slices (81.12.a / b / c / d) populate this
//! registry at boot; `run_plugin_init_loop_with_factory` then
//! consumes it to instantiate + initialize each plugin.
//!
//! Today (81.12.0): infrastructure only. `main.rs`'s legacy plugin
//! registration block is untouched. Operators pass `None` for the
//! `factory_registry` argument of `wire_plugin_registry`; behavior
//! is identical to before.

use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

use nexo_plugin_manifest::PluginManifest;

use crate::agent::plugin_host::NexoPlugin;

/// Phase 81.12.0 — type-erased error returned by plugin factory
/// closures. Each plugin author can use their own error type;
/// the registry boxes them so storage stays homogeneous.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Phase 81.12.0 — closure that constructs an `Arc<dyn NexoPlugin>`
/// from a parsed manifest. The closure typically captures
/// references to the operator's `&AppConfig` (or specific config
/// slices) at registration time so it can build plugin-specific
/// config structs from the running daemon's state.
pub type PluginFactory =
    Box<dyn Fn(&PluginManifest) -> Result<Arc<dyn NexoPlugin>, BoxError> + Send + Sync + 'static>;

/// Phase 81.12.0 — factory map keyed by `plugin.id`. Per-plugin
/// migrations populate this at boot; the
/// `run_plugin_init_loop_with_factory` helper consults it to
/// instantiate concrete plugins from discovered manifests.
#[derive(Default)]
pub struct PluginFactoryRegistry {
    factories: BTreeMap<String, PluginFactory>,
}

impl std::fmt::Debug for PluginFactoryRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginFactoryRegistry")
            .field("kinds", &self.factories.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl PluginFactoryRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a factory under `plugin_id`. First-registers-wins:
    /// duplicate registration returns `Err(AlreadyRegistered)` and
    /// the prior factory stays in place.
    pub fn register(
        &mut self,
        plugin_id: impl Into<String>,
        factory: PluginFactory,
    ) -> Result<(), FactoryRegistrationError> {
        let id = plugin_id.into();
        if self.factories.contains_key(&id) {
            return Err(FactoryRegistrationError::AlreadyRegistered { plugin_id: id });
        }
        self.factories.insert(id, factory);
        Ok(())
    }

    /// Instantiate the plugin registered under `plugin_id` by
    /// calling its factory closure with the parsed `manifest`.
    pub fn instantiate(
        &self,
        plugin_id: &str,
        manifest: &PluginManifest,
    ) -> Result<Arc<dyn NexoPlugin>, FactoryInstantiateError> {
        let factory = self.factories.get(plugin_id).ok_or_else(|| {
            FactoryInstantiateError::NotRegistered {
                plugin_id: plugin_id.to_string(),
            }
        })?;
        factory(manifest).map_err(|source| FactoryInstantiateError::FactoryFailed {
            plugin_id: plugin_id.to_string(),
            source,
        })
    }

    pub fn is_registered(&self, plugin_id: &str) -> bool {
        self.factories.contains_key(plugin_id)
    }

    /// Sorted list of registered plugin ids. Stable iteration for
    /// observability + tests.
    pub fn kinds(&self) -> Vec<String> {
        self.factories.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.factories.len()
    }

    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
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

    use crate::agent::plugin_host::{
        PluginInitContext, PluginInitError, PluginShutdownError,
    };

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
        async fn init(
            &self,
            _ctx: &mut PluginInitContext<'_>,
        ) -> Result<(), PluginInitError> {
            Ok(())
        }
        async fn shutdown(&self) -> Result<(), PluginShutdownError> {
            Ok(())
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
        let mut reg = PluginFactoryRegistry::new();
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
        let mut reg = PluginFactoryRegistry::new();
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
        let mut reg = PluginFactoryRegistry::new();
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
        let mut reg = PluginFactoryRegistry::new();
        let factory: PluginFactory =
            Box::new(|_m| Ok(Arc::new(MockPlugin::build("alpha"))));
        reg.register("alpha", factory).unwrap();
        let m = fixture_manifest("alpha");
        match reg.instantiate("alpha", &m) {
            Ok(handle) => assert_eq!(handle.manifest().plugin.id, "alpha"),
            Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }
}
