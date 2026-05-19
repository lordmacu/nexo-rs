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

use std::sync::Arc;

use nexo_broker::AnyBroker;
use nexo_core::agent::admin_rpc::domains::pairing::PairingChallengeStore;
use nexo_core::agent::admin_rpc::pairing_trigger::PairingChannelTriggers;
use nexo_core::agent::admin_rpc::BrokerPairingTrigger;
use nexo_core::agent::NexoPlugin;
use nexo_pairing::plugin_admin::PluginAdminRouter;
use nexo_pairing::plugin_poller::{PluginPollerHandle, PluginPollerRouter};

use crate::admin_adapters::SharedPluginHandles;

/// Bundle of Arc-shared registries the post-wire cascade touches.
/// One per daemon process — cloned (cheap, all Arc) into the
/// `LivePluginInstaller` AND used inline in `main.rs`'s boot
/// cascade so the helper produces an identical state.
#[derive(Clone)]
pub struct HotPluginCtx {
    pub plugin_handles: SharedPluginHandles,
    pub plugin_admin_router: Arc<PluginAdminRouter>,
    pub plugin_poller_router: Arc<PluginPollerRouter>,
    pub pairing_triggers: PairingChannelTriggers,
    pub pairing_store: Option<Arc<dyn PairingChallengeStore>>,
    pub broker: AnyBroker,
}

impl std::fmt::Debug for HotPluginCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotPluginCtx")
            .field("plugin_handles", &"<shared-cell>")
            .field("plugin_admin_router", &"<router>")
            .field("plugin_poller_router", &"<router>")
            .field("pairing_triggers", &"<triggers>")
            .field(
                "pairing_store",
                &self.pairing_store.as_ref().map(|_| "<store>"),
            )
            .field("broker", &"<broker>")
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
    if removed {
        tracing::info!(
            plugin = %plugin_id,
            "hot-remove: dropped plugin handle from shared cell \
             (admin router + pairing trigger entries linger until re-install)"
        );
    }
    Ok(removed)
}
