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
pub async fn run_plugin_init_loop<F>(
    snapshot: &NexoPluginRegistrySnapshot,
    handles: &BTreeMap<String, Arc<dyn NexoPlugin>>,
    mut ctx_factory: F,
) -> BTreeMap<String, InitOutcome>
where
    F: for<'a> FnMut(&'a str) -> PluginInitContext<'a>,
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
}
