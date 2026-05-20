//! Phase 97.1 — runtime plugin hot-spawn / hot-remove helpers.
//!
//! Mirrors the post-`wire_plugin_registry` registration cascade in
//! `src/main.rs` (Stage 4 admin router, Stage 6 pairing trigger +
//! inbound subscriber, Stage 7 poller router) for a single plugin
//! at runtime. Operator hits `nexo/admin/plugins/scan` /
//! `…/install` → the daemon invokes [`register_plugin_in_live_runtime`]
//! per newly-discovered plugin handle and the channel becomes
//! pairable + dispatchable without a daemon restart.
//!
//! Inverse path is [`unregister_plugin_from_live_runtime`] — drops
//! the handle from the shared cell + best-effort tears down the
//! routers (admin router has no remove API today; the entry stays
//! but the dispatch hits "plugin handle dropped" — informative but
//! non-fatal).
//!
//! The 7-registration boot list narrows to **5 critical** for hot
//! ops; metrics descriptors + HTTP routes + public tunnels stay
//! immutable post-boot because their hosting Vec / Arc lacks
//! interior mutability today. Channel plugins (whatsapp / telegram
//! / email / google) don't need those three, so the gap is purely
//! informational for now — a follow-up converts them to ArcSwap /
//! RwLock when a dashboard plugin needs hot-install.

use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;
use nexo_broker::AnyBroker;
use nexo_core::agent::admin_rpc::domains::pairing::PairingChallengeStore;
use nexo_core::agent::admin_rpc::pairing_trigger::PairingChannelTriggers;
use nexo_core::agent::admin_rpc::BrokerPairingTrigger;
use nexo_core::agent::nexo_plugin_registry::{
    DiscoveredPlugin, NexoPluginRegistry, PluginSkillSummary, SharedPluginSkills,
};
use nexo_core::agent::skills::read_skill_metadata;
use nexo_core::agent::NexoPlugin;
use nexo_pairing::plugin_admin::PluginAdminRouter;
use nexo_pairing::plugin_http::PluginHttpRouter;
use nexo_pairing::plugin_metrics::PluginMetricsDescriptor;
use nexo_pairing::plugin_poller::{PluginPollerHandle, PluginPollerRouter};
use nexo_plugin_manifest::PluginManifest;

use crate::admin_adapters::SharedPluginHandles;

/// Live, hot-swappable list of plugin Prometheus-metrics descriptors
/// (Phase 97.1.γ). Same shape as `RuntimeHealth.plugin_metrics` in
/// `main.rs`; install/uninstall append / drop without a restart.
pub type SharedPluginMetrics = Arc<ArcSwap<Vec<PluginMetricsDescriptor>>>;

/// Bundle of Arc-shared registries the post-wire cascade touches.
/// One per daemon process — cloned (cheap, all Arc) into the
/// `LivePluginInstaller` AND used inline in `main.rs`'s boot
/// cascade so the helper produces an identical state.
#[derive(Clone)]
pub struct HotPluginCtx {
    pub plugin_handles: SharedPluginHandles,
    pub plugin_admin_router: Arc<PluginAdminRouter>,
    /// Live HTTP route table (Phase 97.1.γ 3b). Interior-mutable so
    /// hot install/uninstall adds/drops the plugin's `[plugin.http]`
    /// route without a restart.
    pub plugin_http_router: Arc<PluginHttpRouter>,
    pub plugin_poller_router: Arc<PluginPollerRouter>,
    pub pairing_triggers: PairingChannelTriggers,
    pub pairing_store: Option<Arc<dyn PairingChallengeStore>>,
    pub broker: AnyBroker,
    /// Live plugin-skills handle, shared with every agent (Phase
    /// 97.1.γ). Install/uninstall swaps it so contributed skills go
    /// live without a restart.
    pub plugin_skills: SharedPluginSkills,
    /// Live plugin Prometheus-metrics descriptors, shared with the
    /// `/metrics` scrape (Phase 97.1.γ).
    pub plugin_metrics: SharedPluginMetrics,
    /// The live plugin registry. Hot install/uninstall swaps its
    /// snapshot so admin-UI (Phase 99), discovery, and capability
    /// views see the new plugin set without a restart (97.1.γ 3a).
    pub registry: Arc<NexoPluginRegistry>,
}

impl std::fmt::Debug for HotPluginCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotPluginCtx")
            .field("plugin_handles", &"<shared-cell>")
            .field("plugin_admin_router", &"<router>")
            .field("plugin_http_router", &"<router>")
            .field("plugin_poller_router", &"<router>")
            .field("pairing_triggers", &"<triggers>")
            .field(
                "pairing_store",
                &self.pairing_store.as_ref().map(|_| "<store>"),
            )
            .field("broker", &"<broker>")
            .field("plugin_skills", &"<arc-swap>")
            .field("plugin_metrics", &"<arc-swap>")
            .field("registry", &"<registry>")
            .finish()
    }
}

/// Register a single freshly-spawned plugin into the daemon's live
/// runtime. Idempotent — re-registering the same plugin replaces
/// its entries (the admin router + pairing triggers + poller
/// router all permit last-write-wins on a `register(...)` call).
///
/// Order matches `src/main.rs` post-wire cascade so a hot-spawn
/// produces the exact same daemon state as a boot-time discover:
///   1. plugin_handles_cell — insert handle (clones BTreeMap then
///      Arc-swaps so in-flight readers see the old or new map
///      atomically).
///   2. plugin_admin_router — `[plugin.admin]` route.
///   3. plugin_poller_router — `[plugin.poller]` handle.
///   4. pairing_triggers — `[plugin.pairing.trigger]` +
///      `[plugin.pairing.adapter]` → `BrokerPairingTrigger` insert.
///   5. inbound subscriber — `plugin.inbound.<channel>.*` →
///      pairing_store update task spawn.
///
/// Returns `Ok(())` on success; per-step soft failures (e.g.
/// reserved-prefix collision on admin route) warn-log and continue.
/// Hard failure (plugin_handles_cell not initialised yet) bubbles
/// as `Err` so the operator sees the boot-window race.
pub async fn register_plugin_in_live_runtime(
    plugin_id: &str,
    handle: Arc<dyn NexoPlugin>,
    ctx: &HotPluginCtx,
) -> anyhow::Result<()> {
    let manifest = handle.manifest().clone();

    // 1. plugin_handles_cell. Clone the current BTreeMap (cheap —
    //    Arc bumps), insert the new entry, swap. RwLock write holds
    //    the cell briefly; in-flight readers either see the old or
    //    new map.
    {
        let mut guard = ctx.plugin_handles.write().await;
        let new_map = match guard.as_ref() {
            Some(existing) => {
                let mut m = (**existing).clone();
                m.insert(plugin_id.to_string(), handle.clone());
                Arc::new(m)
            }
            None => {
                anyhow::bail!(
                    "plugin_handles cell not yet populated (daemon still booting); \
                     retry after boot completes"
                );
            }
        };
        *guard = Some(new_map);
    }

    // 2. Plugin admin router.
    if let Some(admin) = manifest.plugin.admin.as_ref() {
        match ctx.plugin_admin_router.register(
            plugin_id,
            &admin.method_prefix,
            &admin.broker_topic_prefix,
            admin.timeout_seconds.map(std::time::Duration::from_secs),
        ) {
            Ok(()) => tracing::info!(
                plugin = %plugin_id,
                method_prefix = %admin.method_prefix,
                "hot-spawn: registered plugin admin route"
            ),
            Err(err) => tracing::warn!(
                plugin = %plugin_id,
                method_prefix = %admin.method_prefix,
                error = %err,
                "hot-spawn: plugin admin route registration rejected"
            ),
        }
    }

    // 2b. Plugin HTTP route (Phase 97.1.γ 3b — interior-mutable).
    if let Some(http) = manifest.plugin.http.as_ref() {
        match ctx.plugin_http_router.register(
            plugin_id,
            &http.mount_prefix,
            http.timeout_seconds.map(std::time::Duration::from_secs),
        ) {
            Ok(()) => tracing::info!(
                plugin = %plugin_id,
                prefix = %http.mount_prefix,
                "hot-spawn: registered plugin HTTP route"
            ),
            Err(err) => tracing::warn!(
                plugin = %plugin_id,
                prefix = %http.mount_prefix,
                error = %err,
                "hot-spawn: plugin HTTP route registration rejected (reserved prefix)"
            ),
        }
    }

    // 3. Plugin poller router.
    if let Some(poller_sec) = manifest.plugin.poller.as_ref() {
        if let Err(err) = poller_sec.validate() {
            tracing::warn!(
                plugin = %plugin_id,
                error = %err,
                "hot-spawn: [plugin.poller] validation failed; skipping"
            );
        } else {
            let new_handle = PluginPollerHandle {
                plugin_id: plugin_id.to_string(),
                kinds: poller_sec.kinds.clone(),
                broker_topic_prefix: poller_sec.broker_topic_prefix.clone(),
                lifecycle: poller_sec.lifecycle,
                max_concurrent_ticks: poller_sec.max_concurrent_ticks,
                tick_timeout: std::time::Duration::from_secs(poller_sec.tick_timeout_secs),
                entrypoint_command: manifest.plugin.entrypoint.command.clone(),
            };
            match ctx.plugin_poller_router.register(new_handle) {
                Ok(()) => tracing::info!(
                    plugin = %plugin_id,
                    kinds = ?poller_sec.kinds,
                    "hot-spawn: registered [plugin.poller]"
                ),
                Err(err) => tracing::warn!(
                    plugin = %plugin_id,
                    error = %err,
                    "hot-spawn: plugin poller registration rejected"
                ),
            }
        }
    }

    // 4 + 5. Pairing trigger + inbound subscriber. Both require
    // `[plugin.pairing.adapter]` + `[plugin.pairing.trigger]` +
    // `[plugin.admin]` (the trigger forwards through the admin
    // broker topic). Mirror the boot-loop's all-three-required
    // contract.
    let pairing_section = &manifest.plugin.pairing;
    if let (Some(adapter), Some(trigger_section), Some(admin_section)) = (
        pairing_section.adapter.as_ref(),
        pairing_section.trigger.as_ref(),
        manifest.plugin.admin.as_ref(),
    ) {
        let broker_trigger = BrokerPairingTrigger::new(
            adapter.channel_id.clone(),
            ctx.broker.clone(),
            trigger_section,
            &admin_section.method_prefix,
            &admin_section.broker_topic_prefix,
        );
        ctx.pairing_triggers
            .insert(adapter.channel_id.clone(), Arc::new(broker_trigger));
        tracing::info!(
            plugin = %plugin_id,
            channel = %adapter.channel_id,
            "hot-spawn: registered broker pairing trigger"
        );
        if let Some(pairing_store) = ctx.pairing_store.as_ref() {
            // Returns JoinHandle (subscriber detached via tokio::spawn);
            // dropping it leaves the task running. drop() instead of
            // `let _` to satisfy clippy::let_underscore_future.
            drop(
                nexo_core::agent::admin_rpc::spawn_pairing_inbound_subscriber(
                    ctx.broker.clone(),
                    adapter.channel_id.clone(),
                    pairing_store.clone(),
                    None,
                ),
            );
            tracing::info!(
                plugin = %plugin_id,
                channel = %adapter.channel_id,
                "hot-spawn: spawned pairing inbound subscriber"
            );
        }
    }

    Ok(())
}

/// Inverse of [`register_plugin_in_live_runtime`]. Drops the handle
/// from the shared cell. The routers don't expose `remove(...)`
/// today; their entries linger but dispatch fails with
/// "plugin handle dropped" until a fresh install replaces them.
/// Sufficient for the uninstall use case — operator wants the
/// plugin OFF more than the router perfectly clean.
pub async fn unregister_plugin_from_live_runtime(
    plugin_id: &str,
    ctx: &HotPluginCtx,
) -> anyhow::Result<bool> {
    let mut guard = ctx.plugin_handles.write().await;
    let removed = match guard.as_ref() {
        Some(existing) => {
            let mut m = (**existing).clone();
            let removed = m.remove(plugin_id).is_some();
            *guard = Some(Arc::new(m));
            removed
        }
        None => false,
    };
    // Phase 97.1.γ 3b — drop the plugin's HTTP route (interior-mutable,
    // unlike the admin/pairing routers which still linger).
    ctx.plugin_http_router.unregister(plugin_id);
    if removed {
        tracing::info!(
            plugin = %plugin_id,
            "hot-remove: dropped plugin handle from shared cell + HTTP route \
             (admin router + pairing trigger entries linger until re-install)"
        );
    }
    Ok(removed)
}

/// Hot-add a plugin's contributed skills to the shared live handle
/// (Phase 97.1.γ). Walks `<root_dir>/<contributes_dir>/<name>/SKILL.md`,
/// caches each skill's frontmatter metadata, and respects
/// first-plugin-wins (a skill name already attributed is left
/// untouched). No-op when the plugin declares no `contributes_dir` or
/// the directory is missing. The next agent turn sees the new skills.
pub fn hot_add_plugin_skills(
    skills: &SharedPluginSkills,
    plugin_id: &str,
    root_dir: &Path,
    manifest: &PluginManifest,
) {
    let Some(dir) = manifest.plugin.skills.contributes_dir.as_ref() else {
        return;
    };
    let root = root_dir.join(dir);
    if !root.exists() {
        return;
    }
    let cur = skills.load_full();
    let mut next = (*cur).clone();
    if !next.roots.contains(&root) {
        next.roots.push(root.clone());
    }
    let Ok(rd) = std::fs::read_dir(&root) else {
        return;
    };
    let mut entries: Vec<_> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        // First-plugin-wins: a name already attributed is not shadowed.
        if next.skills.iter().any(|s| s.name == name) {
            continue;
        }
        let meta = read_skill_metadata(&skill_md, &name);
        next.skills.push(PluginSkillSummary {
            name,
            display_name: meta.name,
            description: meta.description,
            plugin_id: plugin_id.to_string(),
        });
    }
    skills.store(Arc::new(next));
}

/// Hot-remove a plugin's skills from the shared handle (by plugin id,
/// since uninstall only has the id). Also prunes any skill root whose
/// directory no longer exists on disk (the uninstalled plugin's dir).
/// NOTE: a skill name this plugin WON in a conflict is not re-promoted
/// to the runner-up here (a full re-walk on next boot/scan fixes that
/// rare case) — see FOLLOWUPS Phase 97.1.γ.
pub fn hot_remove_plugin_skills(skills: &SharedPluginSkills, plugin_id: &str) {
    let cur = skills.load_full();
    let mut next = (*cur).clone();
    next.skills.retain(|s| s.plugin_id != plugin_id);
    next.roots.retain(|r| r.exists());
    skills.store(Arc::new(next));
}

/// Hot-add a plugin's `[plugin.metrics]` descriptor to the shared
/// live handle so the next `/metrics` scrape includes it (Phase
/// 97.1.γ). No-op unless `prometheus = true`; idempotent.
pub fn hot_add_plugin_metrics(
    metrics: &SharedPluginMetrics,
    plugin_id: &str,
    manifest: &PluginManifest,
) {
    let Some(m) = manifest.plugin.metrics.as_ref() else {
        return;
    };
    if !m.prometheus {
        return;
    }
    let cur = metrics.load_full();
    if cur.iter().any(|d| d.plugin_id == plugin_id) {
        return;
    }
    let mut next = (*cur).clone();
    let mut d = PluginMetricsDescriptor::new(plugin_id, m.broker_topic_prefix.clone());
    if let Some(secs) = m.timeout_seconds {
        d = d.with_timeout(std::time::Duration::from_secs(secs));
    }
    next.push(d);
    metrics.store(Arc::new(next));
}

/// Hot-remove a plugin's metrics descriptor from the shared handle.
pub fn hot_remove_plugin_metrics(metrics: &SharedPluginMetrics, plugin_id: &str) {
    let cur = metrics.load_full();
    let mut next = (*cur).clone();
    next.retain(|d| d.plugin_id != plugin_id);
    metrics.store(Arc::new(next));
}

/// Add newly-installed plugins to the live registry snapshot so the
/// admin-UI aggregator (Phase 99 `plugin_ui/list`), discovery, and
/// capability views see them without a daemon restart (97.1.γ 3a).
/// Dedups by plugin id (idempotent).
pub fn hot_add_plugins_to_snapshot(registry: &NexoPluginRegistry, new: &[DiscoveredPlugin]) {
    if new.is_empty() {
        return;
    }
    let cur = registry.snapshot();
    let mut next = (*cur).clone();
    for dp in new {
        let id = &dp.manifest.plugin.id;
        if next.plugins.iter().any(|p| &p.manifest.plugin.id == id) {
            continue;
        }
        next.plugins.push(dp.clone());
    }
    registry.swap(Arc::new(next));
}

/// Remove a plugin from the live registry snapshot (inverse of
/// [`hot_add_plugins_to_snapshot`]).
pub fn hot_remove_plugin_from_snapshot(registry: &NexoPluginRegistry, plugin_id: &str) {
    let cur = registry.snapshot();
    let mut next = (*cur).clone();
    let before = next.plugins.len();
    next.plugins.retain(|p| p.manifest.plugin.id != plugin_id);
    if next.plugins.len() != before {
        registry.swap(Arc::new(next));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::agent::nexo_plugin_registry::PluginSkillsState;
    use std::fs;

    fn manifest_with(extra: &str) -> PluginManifest {
        let toml = format!(
            "[plugin]\nid = \"alpha\"\nversion = \"0.1.0\"\nname = \"alpha\"\n\
             description = \"x\"\nmin_nexo_version = \">=0.0.1\"\n{extra}"
        );
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn hot_add_then_remove_skills_mutates_shared_handle() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("triager");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: triage things\n---\n\nbody\n",
        )
        .unwrap();
        let manifest = manifest_with("[plugin.skills]\ncontributes_dir = \"skills\"\n");
        let skills = PluginSkillsState::empty().into_shared();

        hot_add_plugin_skills(&skills, "alpha", dir.path(), &manifest);
        let st = skills.load();
        assert_eq!(st.skills.len(), 1);
        assert_eq!(st.skills[0].name, "triager");
        assert_eq!(st.skills[0].plugin_id, "alpha");
        assert_eq!(st.skills[0].description.as_deref(), Some("triage things"));
        assert_eq!(st.roots.len(), 1);
        drop(st);

        // Uninstall deletes the plugin dir; remove drops skills by id
        // and prunes the now-missing root.
        fs::remove_dir_all(dir.path().join("skills")).unwrap();
        hot_remove_plugin_skills(&skills, "alpha");
        let st = skills.load();
        assert!(st.skills.is_empty());
        assert!(st.roots.is_empty());
    }

    #[test]
    fn hot_add_skills_respects_first_plugin_wins() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("dup");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "body\n").unwrap();
        let manifest = manifest_with("[plugin.skills]\ncontributes_dir = \"skills\"\n");
        let skills = PluginSkillsState::empty().into_shared();
        hot_add_plugin_skills(&skills, "alpha", dir.path(), &manifest);
        // Second plugin with the same skill name must NOT shadow alpha.
        hot_add_plugin_skills(&skills, "beta", dir.path(), &manifest);
        let st = skills.load();
        let dups: Vec<_> = st.skills.iter().filter(|s| s.name == "dup").collect();
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].plugin_id, "alpha");
    }

    #[test]
    fn hot_add_metrics_idempotent_then_remove() {
        let manifest = manifest_with(
            "[plugin.metrics]\nprometheus = true\nbroker_topic_prefix = \"plugin.metrics.alpha\"\n",
        );
        let metrics: SharedPluginMetrics = Arc::new(ArcSwap::from_pointee(Vec::new()));
        hot_add_plugin_metrics(&metrics, "alpha", &manifest);
        assert_eq!(metrics.load().len(), 1);
        hot_add_plugin_metrics(&metrics, "alpha", &manifest);
        assert_eq!(metrics.load().len(), 1, "idempotent");
        hot_remove_plugin_metrics(&metrics, "alpha");
        assert_eq!(metrics.load().len(), 0);
    }

    #[test]
    fn snapshot_add_idempotent_then_remove() {
        use nexo_core::agent::nexo_plugin_registry::{DiscoveredPlugin, NexoPluginRegistry};
        let registry = NexoPluginRegistry::empty();
        let dp = DiscoveredPlugin {
            manifest: manifest_with(""),
            root_dir: std::path::PathBuf::from("/tmp/alpha"),
            manifest_path: std::path::PathBuf::from("/tmp/alpha/nexo-plugin.toml"),
        };
        hot_add_plugins_to_snapshot(&registry, std::slice::from_ref(&dp));
        assert_eq!(registry.snapshot().plugins.len(), 1);
        hot_add_plugins_to_snapshot(&registry, std::slice::from_ref(&dp));
        assert_eq!(registry.snapshot().plugins.len(), 1, "idempotent by id");
        hot_remove_plugin_from_snapshot(&registry, "alpha");
        assert!(registry.snapshot().plugins.is_empty());
    }

    #[test]
    fn hot_add_metrics_skips_when_prometheus_false() {
        let manifest =
            manifest_with("[plugin.metrics]\nprometheus = false\nbroker_topic_prefix = \"x\"\n");
        let metrics: SharedPluginMetrics = Arc::new(ArcSwap::from_pointee(Vec::new()));
        hot_add_plugin_metrics(&metrics, "alpha", &manifest);
        assert!(metrics.load().is_empty());
    }
}
