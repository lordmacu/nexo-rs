//! Phase 81.6 — sequential `NexoPlugin::init()` driver. Each plugin
//! gets its outcome recorded; a single failure logs a warn and the
//! loop continues. 81.6 ships with an empty handles map (every
//! plugin records `NoHandle`); 81.7+ will populate it.
//!
//! `tokio::spawn` / panic-catch sandbox is intentionally NOT used —
//! callers assume `init()` is well-behaved. If a real plugin starts
//! misbehaving, the follow-up wraps each call in `tokio::time::timeout`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use crate::agent::plugin_host::{NexoPlugin, PluginInitContext};

use super::factory::{FactoryInstantiateError, PluginFactoryRegistry};
use super::subprocess::subprocess_plugin_factory;
use super::NexoPluginRegistrySnapshot;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum InitOutcome {
    Ok { duration_ms: u64 },
    Failed { error: String },
    /// 81.6 placeholder — the manifest declares a plugin but no
    /// concrete `NexoPlugin` handle was produced. Manifest-driven
    /// instantiation lands in Phase 81.7.
    NoHandle,
}

/// Drive `NexoPlugin::init()` once per plugin in registry order.
/// Sequential — single failure logs warn + records `Failed`; the
/// loop never aborts. Plugins absent from `handles` record
/// `NoHandle` (the common case until 81.7 ships).
///
/// `ctx_factory` is a closure invoked once per plugin id that
/// constructs a fresh [`PluginInitContext`]. The closure must be
/// callable but is never called for plugins recording `NoHandle`,
/// so 81.6 callers can pass an `unreachable!()` body.
pub async fn run_plugin_init_loop<'env, F>(
    snapshot: &NexoPluginRegistrySnapshot,
    handles: &BTreeMap<String, Arc<dyn NexoPlugin>>,
    mut ctx_factory: F,
) -> BTreeMap<String, InitOutcome>
where
    F: FnMut(&str) -> PluginInitContext<'env>,
{
    let mut outcomes = BTreeMap::new();
    for plugin in &snapshot.plugins {
        let id = plugin.manifest.plugin.id.clone();
        let Some(handle) = handles.get(&id).cloned() else {
            outcomes.insert(id, InitOutcome::NoHandle);
            continue;
        };
        let mut ctx = ctx_factory(&id);
        let start = Instant::now();
        match handle.init(&mut ctx).await {
            Ok(()) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                outcomes.insert(
                    id,
                    InitOutcome::Ok { duration_ms },
                );
            }
            Err(e) => {
                let error = e.to_string();
                tracing::warn!(
                    target: "plugins.init",
                    plugin_id = %id,
                    error = %error,
                    "plugin init failed; continuing"
                );
                outcomes.insert(id, InitOutcome::Failed { error });
            }
        }
    }
    outcomes
}

/// Phase 81.12.0 — factory-driven init loop. For each plugin in
/// the snapshot, look up a factory in `factory_registry`; if
/// found, instantiate via the factory closure + call its `init()`.
/// Plugins WITHOUT a factory record `InitOutcome::NoHandle` so
/// operators with partial migration see consistent output during
/// the 81.12.a-e per-plugin slices. Sequential per snapshot order;
/// one failure logs `tracing::warn!` and the loop never aborts.
/// Phase 81.17.b — return shape for the factory-driven init loop.
/// Outcomes describe what happened per plugin id; `handles` carries
/// the live `Arc<dyn NexoPlugin>` instances that successfully
/// passed `init()`. Callers MUST retain `handles` for the daemon's
/// lifetime — dropping them triggers `Arc::drop` on the underlying
/// plugin which, for `SubprocessNexoPlugin`, SIGKILLs the child
/// process via `tokio::process::Command::kill_on_drop(true)`.
pub struct FactoryInitResult {
    pub outcomes: BTreeMap<String, InitOutcome>,
    pub handles: BTreeMap<String, Arc<dyn NexoPlugin>>,
}

pub async fn run_plugin_init_loop_with_factory<'env, F>(
    snapshot: &NexoPluginRegistrySnapshot,
    factory_registry: &PluginFactoryRegistry,
    mut ctx_factory: F,
) -> FactoryInitResult
where
    F: FnMut(&str) -> PluginInitContext<'env>,
{
    let mut outcomes = BTreeMap::new();
    let mut handles: BTreeMap<String, Arc<dyn NexoPlugin>> = BTreeMap::new();
    for plugin in &snapshot.plugins {
        let id = plugin.manifest.plugin.id.clone();
        // Phase 81.17 — auto-subprocess fallback. If no in-tree
        // factory was registered for this id BUT the manifest
        // declares an `[plugin.entrypoint]` with a non-empty
        // `command`, build a `SubprocessNexoPlugin` factory inline
        // and use it. Operator-registered factories take priority
        // — they're the override path for in-tree migrations.
        // Manifests without entrypoint.command keep recording
        // NoHandle (the partial-migration shape from 81.12.a-d).
        if !factory_registry.is_registered(&id) {
            if plugin.manifest.plugin.entrypoint.is_subprocess() {
                let auto_factory = subprocess_plugin_factory(plugin.manifest.clone());
                match auto_factory(&plugin.manifest) {
                    Ok(handle) => {
                        let mut ctx = ctx_factory(&id);
                        let start = std::time::Instant::now();
                        match handle.init(&mut ctx).await {
                            Ok(()) => {
                                let duration_ms = start.elapsed().as_millis() as u64;
                                outcomes
                                    .insert(id.clone(), InitOutcome::Ok { duration_ms });
                                handles.insert(id, handle);
                            }
                            Err(e) => {
                                let error = e.to_string();
                                tracing::warn!(
                                    target: "plugins.init",
                                    plugin_id = %id,
                                    %error,
                                    "subprocess plugin init failed; continuing"
                                );
                                outcomes.insert(id, InitOutcome::Failed { error });
                            }
                        }
                    }
                    Err(source) => {
                        let error = format!("auto-subprocess factory failed: {source}");
                        tracing::warn!(
                            target: "plugins.init",
                            plugin_id = %id,
                            %error,
                            "auto-subprocess plugin construction failed"
                        );
                        outcomes.insert(id, InitOutcome::Failed { error });
                    }
                }
                continue;
            }
            outcomes.insert(id, InitOutcome::NoHandle);
            continue;
        }
        match factory_registry.instantiate(&id, &plugin.manifest) {
            Err(FactoryInstantiateError::NotRegistered { .. }) => {
                outcomes.insert(id, InitOutcome::NoHandle);
            }
            Err(FactoryInstantiateError::FactoryFailed { source, .. }) => {
                let error = format!("factory failed: {source}");
                tracing::warn!(
                    target: "plugins.init",
                    plugin_id = %id,
                    %error,
                    "plugin factory failed; recording Failed outcome"
                );
                outcomes.insert(id, InitOutcome::Failed { error });
            }
            Ok(handle) => {
                let mut ctx = ctx_factory(&id);
                let start = std::time::Instant::now();
                match handle.init(&mut ctx).await {
                    Ok(()) => {
                        let duration_ms = start.elapsed().as_millis() as u64;
                        outcomes.insert(id.clone(), InitOutcome::Ok { duration_ms });
                        handles.insert(id, handle);
                    }
                    Err(e) => {
                        let error = e.to_string();
                        tracing::warn!(
                            target: "plugins.init",
                            plugin_id = %id,
                            %error,
                            "plugin init failed; continuing"
                        );
                        outcomes.insert(id, InitOutcome::Failed { error });
                    }
                }
            }
        }
    }
    FactoryInitResult { outcomes, handles }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use nexo_plugin_manifest::PluginManifest;

    use super::super::report::PluginDiscoveryReport;
    use super::super::DiscoveredPlugin;

    fn discovered(plugin_id: &str) -> DiscoveredPlugin {
        let raw = format!(
            "[plugin]\n\
             id = \"{plugin_id}\"\n\
             version = \"0.1.0\"\n\
             name = \"{plugin_id}\"\n\
             description = \"fixture\"\n\
             min_nexo_version = \">=0.0.1\"\n",
        );
        let manifest: PluginManifest = toml::from_str(&raw).unwrap();
        DiscoveredPlugin {
            manifest,
            root_dir: PathBuf::from("/tmp/fake"),
            manifest_path: PathBuf::from("/tmp/fake/nexo-plugin.toml"),
        }
    }

    fn snapshot_with(plugins: Vec<DiscoveredPlugin>) -> NexoPluginRegistrySnapshot {
        NexoPluginRegistrySnapshot {
            plugins,
            last_report: PluginDiscoveryReport::default(),
            skill_roots: std::collections::BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn init_loop_records_no_handle_when_handles_empty() {
        let snap = snapshot_with(vec![discovered("a"), discovered("b")]);
        let outcomes = run_plugin_init_loop(
            &snap,
            &BTreeMap::new(),
            |_| -> PluginInitContext<'_> {
                unreachable!("ctx_factory should not be called when handles is empty");
            },
        )
        .await;
        assert_eq!(outcomes.len(), 2);
        assert!(matches!(outcomes.get("a"), Some(InitOutcome::NoHandle)));
        assert!(matches!(outcomes.get("b"), Some(InitOutcome::NoHandle)));
    }

    #[test]
    fn init_outcome_serializes_to_json() {
        // Smoke check the wire format the doctor CLI + admin-ui rely
        // on: typed enum with a `outcome` discriminator + snake_case
        // variants.
        let ok = InitOutcome::Ok { duration_ms: 12 };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"outcome\":\"ok\""));
        assert!(s.contains("\"duration_ms\":12"));

        let failed = InitOutcome::Failed { error: "boom".into() };
        let s = serde_json::to_string(&failed).unwrap();
        assert!(s.contains("\"outcome\":\"failed\""));
        assert!(s.contains("\"error\":\"boom\""));

        let none = InitOutcome::NoHandle;
        let s = serde_json::to_string(&none).unwrap();
        assert!(s.contains("\"outcome\":\"no_handle\""));
    }

    /// Phase 81.12.0 — factory-driven init loop records `Failed`
    /// for plugins whose factory closure errors and `NoHandle` for
    /// the unregistered ones. We use a closure that returns Err so
    /// the helper short-circuits BEFORE invoking `ctx_factory` —
    /// building a real `PluginInitContext` is heavy and not
    /// required to validate the dispatch path.
    #[tokio::test]
    async fn run_plugin_init_loop_with_factory_routes_registered_vs_unregistered() {
        use super::super::factory::{PluginFactory, PluginFactoryRegistry};
        use crate::agent::plugin_host::PluginInitContext;

        let snap = snapshot_with(vec![discovered("alpha"), discovered("beta")]);
        let mut registry = PluginFactoryRegistry::new();
        let factory: PluginFactory = Box::new(|_m| {
            let err: super::super::factory::BoxError =
                Box::new(std::io::Error::other("forced failure for test"));
            Err(err)
        });
        registry.register("alpha", factory).unwrap();

        let result = run_plugin_init_loop_with_factory(
            &snap,
            &registry,
            |_id| -> PluginInitContext<'_> {
                unreachable!(
                    "ctx_factory must NOT be invoked when the factory closure returns Err"
                )
            },
        )
        .await;

        // alpha registered → factory failed → Failed outcome.
        match result.outcomes.get("alpha") {
            Some(InitOutcome::Failed { error }) => {
                assert!(error.contains("forced failure"));
            }
            other => panic!(
                "alpha must be Failed (factory closure errored), got {other:?}"
            ),
        }
        // beta unregistered → NoHandle.
        assert!(matches!(
            result.outcomes.get("beta"),
            Some(InitOutcome::NoHandle)
        ));
        // No handles ever produced (factory short-circuited; no
        // init() call).
        assert!(result.handles.is_empty());
    }

    /// Phase 81.17 — auto-subprocess fallback wires
    /// `subprocess_plugin_factory(manifest)` inline when no in-tree
    /// factory is registered for a manifest with
    /// `entrypoint.command`. We verify the factory itself produces a
    /// usable `Arc<dyn NexoPlugin>` via the standalone helper —
    /// end-to-end coverage of the init-loop fallback (with real
    /// spawn + handshake) is in
    /// `crates/core/tests/subprocess_plugin_e2e.rs` because building
    /// a complete `PluginInitContext` for a unit test is heavier
    /// than the fallback logic itself warrants.
    #[tokio::test]
    async fn auto_subprocess_factory_produces_usable_handle() {
        let mut manifest = discovered("auto_subproc").manifest;
        manifest.plugin.entrypoint = nexo_plugin_manifest::EntrypointSection {
            command: Some("/bin/true".to_string()),
            ..Default::default()
        };
        let factory = subprocess_plugin_factory(manifest.clone());
        match factory(&manifest) {
            Ok(plugin) => assert_eq!(plugin.manifest().plugin.id, "auto_subproc"),
            Err(e) => panic!("auto-subprocess factory must build handle, got {e}"),
        }
    }

    /// Phase 81.17 — manifests WITHOUT `entrypoint.command` keep
    /// their pre-81.17 `NoHandle` outcome. Empty entrypoint section
    /// is the in-tree-plugin shape (browser/telegram/whatsapp/email
    /// dormant manifests in 81.12.a-d) — those must NOT accidentally
    /// be instantiated as subprocesses.
    #[tokio::test]
    async fn auto_subprocess_fallback_skips_manifests_without_entrypoint() {
        use super::super::factory::PluginFactoryRegistry;

        // discovered() builds a manifest WITHOUT entrypoint, which
        // serde fills with EntrypointSection::default() — command =
        // None, is_subprocess() == false.
        let snap = snapshot_with(vec![discovered("in_tree_only")]);
        let registry = PluginFactoryRegistry::new();
        let result = run_plugin_init_loop_with_factory(
            &snap,
            &registry,
            |_id| -> PluginInitContext<'_> {
                unreachable!(
                    "ctx_factory must NOT be invoked for non-subprocess manifests"
                )
            },
        )
        .await;
        assert!(
            matches!(
                result.outcomes.get("in_tree_only"),
                Some(InitOutcome::NoHandle)
            ),
            "in-tree manifest without entrypoint must record NoHandle"
        );
        assert!(result.handles.is_empty(), "no handles for NoHandle outcome");
    }
}
