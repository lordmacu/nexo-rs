//! Sequential `NexoPlugin::init()` driver. Each plugin gets its
//! outcome recorded; a single failure logs a warn and the loop
//! continues. Plugins without a constructed handle record `NoHandle`.
//!
//! `tokio::spawn` / panic-catch sandbox is intentionally NOT used —
//! callers assume `init()` is well-behaved.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use nexo_plugin_manifest::PluginManifest;
use serde_yaml::Value;

use crate::agent::plugin_config_loader::{config_error_kind, load_plugin_config};
use crate::agent::plugin_host::{NexoPlugin, PluginConfigureError, PluginInitContext};
use crate::agent::scoped_tool_registry::{NamespaceEnforcement, NamespaceViolation};

use super::factory::{FactoryInstantiateError, PluginFactoryRegistry};
use super::subprocess::subprocess_plugin_factory;
use super::NexoPluginRegistrySnapshot;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum InitOutcome {
    Ok {
        duration_ms: u64,
    },
    Failed {
        error: String,
    },
    /// The manifest declares a plugin but no concrete `NexoPlugin`
    /// handle was produced (no matching factory).
    NoHandle,
}

/// Drive `NexoPlugin::init()` once per plugin in registry order.
/// Sequential — single failure logs warn + records `Failed`; the
/// loop never aborts. Plugins absent from `handles` record `NoHandle`.
///
/// `ctx_factory` is a closure invoked once per plugin id that
/// constructs a fresh [`PluginInitContext`]. The closure is never
/// called for plugins recording `NoHandle`, so callers without
/// handles can pass an `unreachable!()` body.
pub async fn run_plugin_init_loop<'env, F>(
    snapshot: &NexoPluginRegistrySnapshot,
    handles: &BTreeMap<String, Arc<dyn NexoPlugin>>,
    mut ctx_factory: F,
) -> BTreeMap<String, InitOutcome>
where
    F: FnMut(&PluginManifest, &Arc<Value>) -> PluginInitContext<'env>,
{
    let mut outcomes = BTreeMap::new();
    for plugin in &snapshot.plugins {
        let id = plugin.manifest.plugin.id.clone();
        let Some(handle) = handles.get(&id).cloned() else {
            outcomes.insert(id, InitOutcome::NoHandle);
            continue;
        };
        let empty_cfg: Arc<Value> = Arc::new(Value::Mapping(serde_yaml::Mapping::new()));
        // Phase 93.2 — configure(value) before init. Schema-fail or
        // plugin-reject routes through InitOutcome::Failed; init is
        // never called when configure errors.
        if let Err(e) = configure_plugin_with_value(&handle, &plugin.manifest, &empty_cfg).await {
            let error = e.to_string();
            tracing::warn!(
                target: "plugins.init",
                plugin_id = %id,
                error = %error,
                "plugin configure failed; continuing"
            );
            outcomes.insert(id, InitOutcome::Failed { error });
            continue;
        }
        let mut ctx = ctx_factory(&plugin.manifest, &empty_cfg);
        let start = Instant::now();
        match handle.init(&mut ctx).await {
            Ok(()) => {
                // Phase 93.7 — `run_plugin_init_loop` (non-factory)
                // doesn't carry a `&CredentialsBundle`; the legacy
                // path is tests/no-handle only. Bundle threading
                // lives in `run_plugin_init_loop_with_factory`.
                let duration_ms = start.elapsed().as_millis() as u64;
                outcomes.insert(id, InitOutcome::Ok { duration_ms });
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

/// Return shape for the factory-driven init loop. For each plugin in
/// the snapshot, look up a factory in `factory_registry`; if found,
/// instantiate via the factory closure + call its `init()`. Plugins
/// WITHOUT a factory record `InitOutcome::NoHandle`. Sequential per
/// snapshot order; one failure logs `tracing::warn!` and the loop
/// never aborts.
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

/// Phase 93.2 — host pre-validation gate for
/// [`NexoPlugin::configure`].
///
/// Returns `Ok(())` when the operator YAML matches the manifest's
/// `[plugin.config_schema]` (Phase 93.1) or when the manifest
/// declares no schema (legacy plugins through the Phase 93.5
/// deprecation window). On failure returns the
/// [`PluginConfigureError`] variant the init-loop wraps into
/// [`InitOutcome::Failed`].
async fn validate_operator_value(
    plugin_id: &str,
    value: &Value,
    manifest: &PluginManifest,
) -> Result<(), PluginConfigureError> {
    let Some(spec) = manifest.plugin.config_schema.as_ref() else {
        return Ok(());
    };
    let schema_json: serde_json::Value =
        serde_json::from_str(&spec.schema).map_err(|e| {
            PluginConfigureError::PluginRejected {
                plugin_id: plugin_id.to_string(),
                source: anyhow::anyhow!(
                    "[plugin.config_schema] schema string is not valid JSON: {e}"
                ),
            }
        })?;
    let value_json: serde_json::Value = serde_json::to_value(value).map_err(|e| {
        PluginConfigureError::PluginRejected {
            plugin_id: plugin_id.to_string(),
            source: anyhow::anyhow!("operator YAML failed YAML→JSON conversion: {e}"),
        }
    })?;
    let errors = nexo_plugin_manifest::validate_config(&value_json, &schema_json);
    if !errors.is_empty() {
        return Err(PluginConfigureError::SchemaValidation {
            plugin_id: plugin_id.to_string(),
            errors,
        });
    }
    Ok(())
}

/// Phase 93.2 — validate the operator slice + call `configure`
/// before `init`. Returns the error from whichever stage failed
/// (the host pre-validator or the plugin's own runtime check).
///
/// Empty operator YAML against a declared schema emits a
/// `tracing::warn!` on the `plugins.init.empty_config` target
/// before delegating to `configure(&Value::Null)`; the plugin
/// decides whether empty is acceptable.
async fn configure_plugin_with_value(
    handle: &Arc<dyn NexoPlugin>,
    manifest: &PluginManifest,
    plugin_cfg: &Value,
) -> Result<(), PluginConfigureError> {
    let plugin_id = manifest.plugin.id.as_str();
    validate_operator_value(plugin_id, plugin_cfg, manifest).await?;
    if manifest.plugin.config_schema.is_some()
        && matches!(plugin_cfg, Value::Null)
            || matches!(plugin_cfg, Value::Mapping(m) if m.is_empty())
    {
        tracing::warn!(
            target: "plugins.init.empty_config",
            plugin_id = %plugin_id,
            "plugin declares [plugin.config_schema] but no config supplied; calling configure with empty value"
        );
        handle.configure(&Value::Null).await
    } else {
        handle.configure(plugin_cfg).await
    }
}

/// Phase 93.7 — collect a plugin's credential store contribution
/// (if any) and insert into `bundle.stores_v2` under the plugin's
/// reported `plugin_id()`. Last-write-wins on collision; emits
/// `tracing::warn!` on overwrite. No-op when `bundle` is `None`
/// (tests that drive the init loop without a real bundle) or when
/// the plugin returns `None` from `credential_store()`.
fn collect_credential_store(
    handle: &Arc<dyn NexoPlugin>,
    bundle: Option<&nexo_auth::wire::CredentialsBundle>,
) {
    let Some(bundle) = bundle else { return };
    let Some(store) = handle.credential_store() else { return };
    let plugin_id = store.plugin_id().to_string();
    if let Some(_replaced) = bundle.stores_v2.insert(plugin_id.clone(), store) {
        tracing::warn!(
            target: "auth.stores.collision",
            plugin_id = %plugin_id,
            "plugin store overwrote existing entry in bundle.stores_v2",
        );
    }
}

/// Convert collected violations into a human-readable
/// "first-3-then-count" sample string. Used by the init loop to
/// enrich `InitOutcome::Failed` when Strict mode rejects.
fn format_violation_sample(violations: &[NamespaceViolation]) -> String {
    let take = violations.len().min(3);
    let head: Vec<String> = violations
        .iter()
        .take(take)
        .map(|v| format!("{}={}", v.attempted_name, v.reason.as_str()))
        .collect();
    if violations.len() > take {
        format!("{} … (+{} more)", head.join(", "), violations.len() - take)
    } else {
        head.join(", ")
    }
}

/// Phase 93.3 — resolve the operator-supplied plugin config slice,
/// preferring `cfg.plugins.entries.<plugin_id>` (built from
/// `<config_dir>/plugins/<plugin_id>.yaml`) over the legacy
/// `<config_dir>/plugins/<plugin_id>/*.yaml` subdir reader.
///
/// Emits a `tracing::info!` on the `plugins.config` target when
/// both pathways would have yielded data (dual-state during the
/// Phase 93.5 deprecation window); the flat-file entries wins.
fn resolve_plugin_cfg(
    plugin_id: &str,
    plugin_root: &Path,
    config_dir: &Path,
    manifest: &PluginManifest,
) -> Result<Arc<Value>, InitOutcome> {
    let plugins_dir = config_dir.join("plugins");
    let entries = nexo_config::load_plugin_entries(&plugins_dir);
    if let Some(value) = entries.get(plugin_id) {
        // Dual-state probe: does the legacy subdir layout also
        // exist? Cheap stat. Emits an info log so operators see
        // the migration state during boot.
        let legacy_subdir = plugins_dir.join(plugin_id);
        if legacy_subdir.is_dir() {
            tracing::info!(
                target: "plugins.config",
                plugin_id = %plugin_id,
                "both cfg.plugins.entries.<id> and legacy plugins/<id>/*.yaml populated; entries wins",
            );
        }
        return Ok(Arc::new(value.clone()));
    }
    try_load_plugin_config(plugin_id, plugin_root, config_dir, manifest)
}

/// Load + validate per-plugin config dir BEFORE `init()` runs. On
/// failure, returns `InitOutcome::Failed` so the caller skips
/// `init()` and records the outcome. `tracing::warn!` is the
/// operator-visible signal.
fn try_load_plugin_config(
    plugin_id: &str,
    plugin_root: &Path,
    config_dir: &Path,
    manifest: &PluginManifest,
) -> Result<Arc<Value>, InitOutcome> {
    match load_plugin_config(plugin_root, config_dir, manifest) {
        Ok(cfg) => Ok(Arc::new(cfg.merged)),
        Err(err) => {
            let kind = config_error_kind(&err);
            let error = err.to_string();
            tracing::warn!(
                target: "plugins.init",
                plugin_id = %plugin_id,
                kind = %kind,
                %error,
                "plugin config load failed; skipping init"
            );
            Err(InitOutcome::Failed {
                error: format!("config load: {error}"),
            })
        }
    }
}

/// After `init()` + channel + LLM + hook registrations succeed,
/// register every backend name from
/// `manifest.plugin.extends.memory_backends` as a
/// `RemoteVectorBackend`. Only fires when the handle is a
/// `SubprocessNexoPlugin`. Failure escalates to
/// `InitOutcome::Failed`.
async fn register_remote_vector_backends_after_init(
    plugin_id: &str,
    handle: &Arc<dyn NexoPlugin>,
    vector_backend_registry: &Arc<crate::agent::vector_backend_registry::VectorBackendRegistry>,
) -> Option<InitOutcome> {
    let any = handle.as_any();
    let sub = match any
        .downcast_ref::<crate::agent::nexo_plugin_registry::subprocess::SubprocessNexoPlugin>()
    {
        Some(s) => s,
        None => return None,
    };
    match sub
        .register_remote_vector_backends(vector_backend_registry)
        .await
    {
        Ok(_) => None,
        Err(e) => {
            let error = format!("vector backend register: {e}");
            tracing::warn!(
                target: "plugins.init",
                plugin_id = %plugin_id,
                %error,
                "remote vector backend registration failed"
            );
            Some(InitOutcome::Failed { error })
        }
    }
}

/// After `init()` + channel + LLM + hook + vector registrations
/// succeed, register every tool name from
/// `manifest.plugin.extends.tools` (intersected with the
/// initialize-reply tools array) as a `RemoteToolHandler` in the
/// per-plugin scoped tool registry. Only fires when the handle
/// is a `SubprocessNexoPlugin`; other concrete types skip
/// silently. Failure escalates to `InitOutcome::Failed` so the
/// agent loop never tries to dispatch a tool whose subprocess
/// registration is broken.
async fn register_remote_tool_handlers_after_init(
    plugin_id: &str,
    handle: &Arc<dyn NexoPlugin>,
    scoped_tool_registry: &Arc<crate::agent::scoped_tool_registry::ScopedToolRegistry>,
) -> Option<InitOutcome> {
    let any = handle.as_any();
    let sub = match any
        .downcast_ref::<crate::agent::nexo_plugin_registry::subprocess::SubprocessNexoPlugin>()
    {
        Some(s) => s,
        None => return None,
    };
    // Synthetic instance plugins (id of form `<base>.<instance>`)
    // share the base plugin's tool catalog because the child binary
    // embeds the base manifest. The base factory already registered
    // every tool name; the instance variant must NOT re-register or
    // it hits "already registered" and aborts init for the per-instance
    // wiring (pairing slots, inbound binding routing). Skip remote
    // tool handler registration for synthetic instances — the tools
    // are reachable through the base plugin's handler anyway.
    if plugin_id.contains('.') {
        return None;
    }
    match sub
        .register_remote_tool_handlers(scoped_tool_registry)
        .await
    {
        Ok(names) => {
            if !names.is_empty() {
                tracing::info!(
                    target: "plugins.init",
                    plugin_id = %plugin_id,
                    registered_count = names.len(),
                    "registered remote tools"
                );
            }
            None
        }
        Err(e) => {
            let error = format!("tool handler register: {e}");
            tracing::warn!(
                target: "plugins.init",
                plugin_id = %plugin_id,
                %error,
                "remote tool handler registration failed"
            );
            Some(InitOutcome::Failed { error })
        }
    }
}

/// After `init()` + channel + LLM registrations succeed, register
/// every hook name from
/// `manifest.plugin.extends.hooks` as a `RemoteHookHandler`.
/// Only fires when the handle is a `SubprocessNexoPlugin`; other
/// concrete types skip silently. Failure escalates to
/// `InitOutcome::Failed`. Continue-on-error semantic lives
/// inside `RemoteHookHandler::on_hook` itself; this hook only
/// fires when registration plumbing fails (e.g. Inner not yet
/// populated — shouldn't happen post-init).
async fn register_remote_hook_handlers_after_init(
    plugin_id: &str,
    handle: &Arc<dyn NexoPlugin>,
    hook_registry: &Arc<crate::agent::hook_registry::HookRegistry>,
) -> Option<InitOutcome> {
    let any = handle.as_any();
    let sub = match any
        .downcast_ref::<crate::agent::nexo_plugin_registry::subprocess::SubprocessNexoPlugin>()
    {
        Some(s) => s,
        None => return None,
    };
    match sub.register_remote_hook_handlers(hook_registry).await {
        Ok(_) => None,
        Err(e) => {
            let error = format!("hook handler register: {e}");
            tracing::warn!(
                target: "plugins.init",
                plugin_id = %plugin_id,
                %error,
                "remote hook handler registration failed"
            );
            Some(InitOutcome::Failed { error })
        }
    }
}

/// After `init()` + channel registration succeed, register every
/// provider name from
/// `manifest.plugin.extends.llm_providers` as a `RemoteLlmFactory`.
/// Only fires when the handle is a `SubprocessNexoPlugin`; other
/// concrete types skip silently. Failure escalates to
/// `InitOutcome::Failed`.
async fn register_remote_llm_providers_after_init(
    plugin_id: &str,
    handle: &Arc<dyn NexoPlugin>,
    llm_registry: &Arc<nexo_llm::LlmRegistry>,
) -> Option<InitOutcome> {
    let any = handle.as_any();
    let sub = match any
        .downcast_ref::<crate::agent::nexo_plugin_registry::subprocess::SubprocessNexoPlugin>()
    {
        Some(s) => s,
        None => return None,
    };
    match sub.register_remote_llm_providers(llm_registry).await {
        Ok(_) => None,
        Err(e) => {
            let error = format!("llm provider register: {e}");
            tracing::warn!(
                target: "plugins.init",
                plugin_id = %plugin_id,
                %error,
                "remote LLM provider registration failed"
            );
            Some(InitOutcome::Failed { error })
        }
    }
}

/// After `init()` returns Ok, register every kind
/// from `manifest.plugin.extends.channels` as a
/// `RemoteChannelAdapter`. Only fires when the handle is a
/// `SubprocessNexoPlugin`; other concrete types skip silently.
/// Failure escalates to `InitOutcome::Failed`.
async fn register_remote_channels_after_init(
    plugin_id: &str,
    handle: &Arc<dyn NexoPlugin>,
    channel_adapter_registry: &Arc<crate::agent::channel_adapter::ChannelAdapterRegistry>,
) -> Option<InitOutcome> {
    let any = handle.as_any();
    let sub = match any
        .downcast_ref::<crate::agent::nexo_plugin_registry::subprocess::SubprocessNexoPlugin>()
    {
        Some(s) => s,
        None => return None,
    };
    match sub
        .register_remote_channel_adapters(channel_adapter_registry)
        .await
    {
        Ok(_) => None,
        Err(e) => {
            let error = format!("channel adapter register: {e}");
            tracing::warn!(
                target: "plugins.init",
                plugin_id = %plugin_id,
                %error,
                "remote channel adapter registration failed"
            );
            Some(InitOutcome::Failed { error })
        }
    }
}

/// After every other post-init step succeeds, spawn the per-plugin
/// auto-respawn supervisor task. Only fires
/// when the handle is a `SubprocessNexoPlugin`; in-tree plugins
/// don't need a respawn loop (they share the daemon's lifetime).
/// Returns `None` always — `spawn_supervisor_loop` is
/// fire-and-forget and can't fail.
fn start_plugin_supervisor_loop_after_init(
    plugin_id: &str,
    handle: &Arc<dyn NexoPlugin>,
    ctx: &PluginInitContext<'_>,
) -> Option<InitOutcome> {
    let any = handle.as_any();
    let sub = match any
        .downcast_ref::<crate::agent::nexo_plugin_registry::subprocess::SubprocessNexoPlugin>()
    {
        Some(s) => s,
        None => return None,
    };
    // Re-derive the LlmServices the same way `init()` did so
    // respawned children speak through the same provider plumbing.
    let llm = Some(
        crate::agent::nexo_plugin_registry::subprocess::LlmServices {
            registry: ctx.llm_registry.clone(),
            config: ctx.llm_config.clone(),
        },
    );
    // The respawn loop needs `Arc<SubprocessNexoPlugin>` (typed,
    // not `Arc<dyn NexoPlugin>`). The factory stashed a
    // `Weak<SubprocessNexoPlugin>` inside the concrete struct
    // immediately after `Arc::new`, before coercing to
    // `Arc<dyn>`; upgrading that Weak gives us the typed Arc
    // back without a clone. If the upgrade fails (only possible
    // for hand-built plugins constructed outside the factory —
    // i.e. tests), skip the supervisor loop silently.
    let Some(arc_sub) = sub.weak_self_arc() else {
        tracing::debug!(
            target: "plugins.init",
            plugin_id = %plugin_id,
            "supervisor loop skipped (no Weak<Self> populated; factory bypassed)"
        );
        return None;
    };
    arc_sub.spawn_supervisor_loop(
        ctx.shutdown.clone(),
        Some(ctx.broker.clone()),
        ctx.long_term_memory.clone(),
        llm,
    );
    tracing::debug!(
        target: "plugins.init",
        plugin_id = %plugin_id,
        "supervisor loop spawned (auto-respawn per manifest.supervisor.respawn)"
    );
    None
}

/// Drain ScopedToolRegistry after `init()` returns and,
/// in Strict mode, escalate any violations to `InitOutcome::Failed`.
/// Returns `None` when the post-init outcome is unchanged.
fn check_namespace_after_init(plugin_id: &str, ctx: &PluginInitContext<'_>) -> Option<InitOutcome> {
    let violations = ctx.tool_registry.drain_violations();
    if violations.is_empty() {
        return None;
    }
    if ctx.tool_registry.mode() != NamespaceEnforcement::Strict {
        // Warn mode — log only; init outcome stays Ok.
        tracing::warn!(
            target: "plugins.init",
            plugin_id = %plugin_id,
            count = violations.len(),
            "plugin tool-namespace violations recorded (warn mode; init succeeded)",
        );
        return None;
    }
    let sample = format_violation_sample(&violations);
    let error = format!(
        "plugin `{plugin_id}` violated tool namespace policy ({} violation(s); first 3: {sample})",
        violations.len()
    );
    tracing::warn!(
        target: "plugins.init",
        plugin_id = %plugin_id,
        count = violations.len(),
        %sample,
        "tool namespace violations rejected (strict mode)",
    );
    Some(InitOutcome::Failed { error })
}

pub async fn run_plugin_init_loop_with_factory<'env, F>(
    snapshot: &NexoPluginRegistrySnapshot,
    factory_registry: &PluginFactoryRegistry,
    config_dir: &Path,
    channel_adapter_registry: &Arc<crate::agent::channel_adapter::ChannelAdapterRegistry>,
    llm_registry: &Arc<nexo_llm::LlmRegistry>,
    hook_registry: &Arc<crate::agent::hook_registry::HookRegistry>,
    vector_backend_registry: &Arc<crate::agent::vector_backend_registry::VectorBackendRegistry>,
    bundle: Option<&nexo_auth::wire::CredentialsBundle>,
    mut ctx_factory: F,
) -> FactoryInitResult
where
    F: FnMut(&PluginManifest, &Arc<Value>) -> PluginInitContext<'env>,
{
    let mut outcomes = BTreeMap::new();
    let mut handles: BTreeMap<String, Arc<dyn NexoPlugin>> = BTreeMap::new();
    for plugin in &snapshot.plugins {
        let id = plugin.manifest.plugin.id.clone();
        // Load + validate the plugin's config dir BEFORE any
        // factory work. Failure here aborts the
        // plugin's load with `InitOutcome::Failed`; the factory
        // closure (and any subsequent init step) never runs.
        // We pre-compute eagerly so both the auto-subprocess
        // fallback and the registered-factory path see a
        // pre-validated config.
        // Phase 93.3 — prefer cfg.plugins.entries.<id> when
        // populated; fall back to the legacy per-plugin subdir
        // reader otherwise. The entries map is rebuilt from the
        // same `<config_dir>/plugins/*.yaml` files so a single
        // operator-supplied flat file feeds both pathways.
        let plugin_cfg = match resolve_plugin_cfg(&id, &plugin.root_dir, config_dir, &plugin.manifest) {
            Ok(cfg) => cfg,
            Err(failed) => {
                outcomes.insert(id, failed);
                continue;
            }
        };
        // Auto-subprocess fallback. If no in-tree factory was
        // registered for this id BUT the manifest declares an
        // `[plugin.entrypoint]` with a non-empty `command`, build a
        // `SubprocessNexoPlugin` factory inline and use it.
        // Operator-registered factories take priority — they're the
        // override path for in-tree migrations. Manifests without
        // entrypoint.command keep recording NoHandle.
        if !factory_registry.is_registered(&id) {
            if plugin.manifest.plugin.entrypoint.is_subprocess() {
                let auto_factory = subprocess_plugin_factory(plugin.manifest.clone());
                match auto_factory(&plugin.manifest) {
                    Ok(handle) => {
                        // Phase 93.2 — configure(value) before init.
                        if let Err(e) =
                            configure_plugin_with_value(&handle, &plugin.manifest, &plugin_cfg)
                                .await
                        {
                            let error = e.to_string();
                            tracing::warn!(
                                target: "plugins.init",
                                plugin_id = %id,
                                error = %error,
                                "plugin configure failed; continuing"
                            );
                            outcomes.insert(id, InitOutcome::Failed { error });
                            continue;
                        }
                        let mut ctx = ctx_factory(&plugin.manifest, &plugin_cfg);
                        let start = std::time::Instant::now();
                        match handle.init(&mut ctx).await {
                            Ok(()) => {
                                // Phase 93.7 — collect plugin's
                                // credential store contribution.
                                collect_credential_store(&handle, bundle);
                                let duration_ms = start.elapsed().as_millis() as u64;
                                if let Some(failed) = check_namespace_after_init(&id, &ctx) {
                                    outcomes.insert(id, failed);
                                } else if let Some(failed) = register_remote_channels_after_init(
                                    &id,
                                    &handle,
                                    channel_adapter_registry,
                                )
                                .await
                                {
                                    outcomes.insert(id, failed);
                                } else if let Some(failed) =
                                    register_remote_llm_providers_after_init(
                                        &id,
                                        &handle,
                                        llm_registry,
                                    )
                                    .await
                                {
                                    outcomes.insert(id, failed);
                                } else if let Some(failed) =
                                    register_remote_hook_handlers_after_init(
                                        &id,
                                        &handle,
                                        hook_registry,
                                    )
                                    .await
                                {
                                    outcomes.insert(id, failed);
                                } else if let Some(failed) =
                                    register_remote_vector_backends_after_init(
                                        &id,
                                        &handle,
                                        vector_backend_registry,
                                    )
                                    .await
                                {
                                    outcomes.insert(id, failed);
                                } else if let Some(failed) =
                                    register_remote_tool_handlers_after_init(
                                        &id,
                                        &handle,
                                        &ctx.tool_registry,
                                    )
                                    .await
                                {
                                    outcomes.insert(id, failed);
                                } else if let Some(failed) =
                                    start_plugin_supervisor_loop_after_init(&id, &handle, &ctx)
                                {
                                    outcomes.insert(id, failed);
                                } else {
                                    outcomes.insert(id.clone(), InitOutcome::Ok { duration_ms });
                                    handles.insert(id, handle);
                                }
                            }
                            Err(e) => {
                                // Walk the `std::error::Error::source` chain so
                                // `PluginInitError::Other { source }` shows its
                                // real cause (the wrapper's Display alone says
                                // only "plugin `whatsapp` init failed").
                                let mut chain = e.to_string();
                                let mut src: Option<&dyn std::error::Error> =
                                    std::error::Error::source(&e);
                                while let Some(cause) = src {
                                    use std::fmt::Write;
                                    let _ = write!(&mut chain, " ← {cause}");
                                    src = cause.source();
                                }
                                tracing::warn!(
                                    target: "plugins.init",
                                    plugin_id = %id,
                                    error = %chain,
                                    "subprocess plugin init failed; continuing"
                                );
                                outcomes.insert(id, InitOutcome::Failed { error: chain });
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
                // Phase 93.2 — configure(value) before init.
                if let Err(e) =
                    configure_plugin_with_value(&handle, &plugin.manifest, &plugin_cfg).await
                {
                    let error = e.to_string();
                    tracing::warn!(
                        target: "plugins.init",
                        plugin_id = %id,
                        error = %error,
                        "plugin configure failed; continuing"
                    );
                    outcomes.insert(id, InitOutcome::Failed { error });
                    continue;
                }
                let mut ctx = ctx_factory(&plugin.manifest, &plugin_cfg);
                let start = std::time::Instant::now();
                match handle.init(&mut ctx).await {
                    Ok(()) => {
                        // Phase 93.7 — collect plugin's credential
                        // store contribution.
                        collect_credential_store(&handle, bundle);
                        let duration_ms = start.elapsed().as_millis() as u64;
                        if let Some(failed) = check_namespace_after_init(&id, &ctx) {
                            outcomes.insert(id, failed);
                        } else if let Some(failed) = register_remote_channels_after_init(
                            &id,
                            &handle,
                            channel_adapter_registry,
                        )
                        .await
                        {
                            outcomes.insert(id, failed);
                        } else if let Some(failed) =
                            register_remote_llm_providers_after_init(&id, &handle, llm_registry)
                                .await
                        {
                            outcomes.insert(id, failed);
                        } else if let Some(failed) =
                            register_remote_hook_handlers_after_init(&id, &handle, hook_registry)
                                .await
                        {
                            outcomes.insert(id, failed);
                        } else if let Some(failed) = register_remote_vector_backends_after_init(
                            &id,
                            &handle,
                            vector_backend_registry,
                        )
                        .await
                        {
                            outcomes.insert(id, failed);
                        } else if let Some(failed) = register_remote_tool_handlers_after_init(
                            &id,
                            &handle,
                            &ctx.tool_registry,
                        )
                        .await
                        {
                            outcomes.insert(id, failed);
                        } else if let Some(failed) =
                            start_plugin_supervisor_loop_after_init(&id, &handle, &ctx)
                        {
                            outcomes.insert(id, failed);
                        } else {
                            outcomes.insert(id.clone(), InitOutcome::Ok { duration_ms });
                            handles.insert(id, handle);
                        }
                    }
                    Err(e) => {
                        // Walk error source chain like the subprocess
                        // path above so PluginInitError::Other surfaces
                        // its real cause (Display alone says only
                        // "plugin `<id>` init failed").
                        let mut chain = e.to_string();
                        let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
                        while let Some(cause) = src {
                            use std::fmt::Write;
                            let _ = write!(&mut chain, " ← {cause}");
                            src = cause.source();
                        }
                        tracing::warn!(
                            target: "plugins.init",
                            plugin_id = %id,
                            error = %chain,
                            "plugin init failed; continuing"
                        );
                        outcomes.insert(id, InitOutcome::Failed { error: chain });
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
            |_m, _cfg| -> PluginInitContext<'_> {
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

        let failed = InitOutcome::Failed {
            error: "boom".into(),
        };
        let s = serde_json::to_string(&failed).unwrap();
        assert!(s.contains("\"outcome\":\"failed\""));
        assert!(s.contains("\"error\":\"boom\""));

        let none = InitOutcome::NoHandle;
        let s = serde_json::to_string(&none).unwrap();
        assert!(s.contains("\"outcome\":\"no_handle\""));
    }

    /// The factory-driven init loop records `Failed`
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

        let cfg_dir = tempfile::tempdir().unwrap();
        let chan_reg = Arc::new(crate::agent::channel_adapter::ChannelAdapterRegistry::new());
        let llm_reg = Arc::new(nexo_llm::LlmRegistry::new());
        let hook_reg = Arc::new(crate::agent::hook_registry::HookRegistry::new());
        let vec_reg = Arc::new(crate::agent::vector_backend_registry::VectorBackendRegistry::new());
        let result = run_plugin_init_loop_with_factory(
            &snap,
            &registry,
            cfg_dir.path(),
            &chan_reg,
            &llm_reg,
            &hook_reg,
            &vec_reg,
            None, // Phase 93.7 bundle (tests don't carry one)
            |_m, _cfg| -> PluginInitContext<'_> {
                unreachable!("ctx_factory must NOT be invoked when the factory closure returns Err")
            },
        )
        .await;

        // alpha registered → factory failed → Failed outcome.
        match result.outcomes.get("alpha") {
            Some(InitOutcome::Failed { error }) => {
                assert!(error.contains("forced failure"));
            }
            other => panic!("alpha must be Failed (factory closure errored), got {other:?}"),
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

    /// The auto-subprocess fallback wires
    /// `subprocess_plugin_factory(manifest)` inline when no in-tree
    /// factory is registered for a manifest with
    /// `entrypoint.command`. We verify the factory itself produces a
    /// usable `Arc<dyn NexoPlugin>` via the standalone helper —
    /// end-to-end coverage of the init-loop fallback (with real
    /// spawn + handshake) is in
    /// `crates/core/tests/subprocess_plugin_e2e.rs` because building
    /// a complete `PluginInitContext` for a unit test is heavier
    /// than the fallback logic itself warrants.
    #[test]
    fn format_violation_sample_truncates_after_three() {
        use crate::agent::scoped_tool_registry::{NamespaceViolation, NamespaceViolationReason};
        let violations = vec![
            NamespaceViolation {
                plugin_id: "p".into(),
                attempted_name: "agent_x".into(),
                reason: NamespaceViolationReason::ReservedPrefix("agent_"),
            },
            NamespaceViolation {
                plugin_id: "p".into(),
                attempted_name: "p_a".into(),
                reason: NamespaceViolationReason::NotInExpose,
            },
            NamespaceViolation {
                plugin_id: "p".into(),
                attempted_name: "p_b".into(),
                reason: NamespaceViolationReason::OutOfNamespace,
            },
            NamespaceViolation {
                plugin_id: "p".into(),
                attempted_name: "p_c".into(),
                reason: NamespaceViolationReason::Collision,
            },
        ];
        let sample = format_violation_sample(&violations);
        assert!(sample.contains("agent_x=ReservedPrefix"));
        assert!(sample.contains("p_a=NotInExpose"));
        assert!(sample.contains("p_b=OutOfNamespace"));
        assert!(sample.contains("(+1 more)"));
        assert!(!sample.contains("p_c"));
    }

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

    /// Manifests WITHOUT `entrypoint.command` keep their `NoHandle`
    /// outcome. An empty entrypoint section is the in-tree-plugin
    /// shape — those must NOT accidentally be instantiated as
    /// subprocesses.
    #[tokio::test]
    async fn auto_subprocess_fallback_skips_manifests_without_entrypoint() {
        use super::super::factory::PluginFactoryRegistry;

        // discovered() builds a manifest WITHOUT entrypoint, which
        // serde fills with EntrypointSection::default() — command =
        // None, is_subprocess() == false.
        let snap = snapshot_with(vec![discovered("in_tree_only")]);
        let registry = PluginFactoryRegistry::new();
        let cfg_dir = tempfile::tempdir().unwrap();
        let chan_reg = Arc::new(crate::agent::channel_adapter::ChannelAdapterRegistry::new());
        let llm_reg = Arc::new(nexo_llm::LlmRegistry::new());
        let hook_reg = Arc::new(crate::agent::hook_registry::HookRegistry::new());
        let vec_reg = Arc::new(crate::agent::vector_backend_registry::VectorBackendRegistry::new());
        let result = run_plugin_init_loop_with_factory(
            &snap,
            &registry,
            cfg_dir.path(),
            &chan_reg,
            &llm_reg,
            &hook_reg,
            &vec_reg,
            None, // Phase 93.7 bundle (tests don't carry one)
            |_m, _cfg| -> PluginInitContext<'_> {
                unreachable!("ctx_factory must NOT be invoked for non-subprocess manifests")
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

    /// When the plugin's manifest declares
    /// `config.schema_path` pointing at a non-existent file,
    /// the loader returns `SchemaRead` and the init-loop
    /// records `InitOutcome::Failed` BEFORE invoking the
    /// closure (closure body is `unreachable!`).
    #[tokio::test]
    async fn init_loop_records_failed_when_config_load_fails() {
        use super::super::factory::PluginFactory;

        let raw = "[plugin]\n\
                   id = \"slack\"\n\
                   version = \"0.1.0\"\n\
                   name = \"slack\"\n\
                   description = \"fixture\"\n\
                   min_nexo_version = \">=0.0.1\"\n\
                   \n\
                   [plugin.config]\n\
                   schema_path = \"missing-schema.json\"\n";
        let manifest: PluginManifest = toml::from_str(raw).unwrap();
        let plugin_root = tempfile::tempdir().unwrap();
        let snap = snapshot_with(vec![DiscoveredPlugin {
            manifest,
            root_dir: plugin_root.path().to_path_buf(),
            manifest_path: plugin_root.path().join("nexo-plugin.toml"),
        }]);

        // Place a yaml file so the loader actually tries to
        // validate (otherwise empty dir → empty config and the
        // schema read fires anyway because schema_path is set).
        let cfg_dir = tempfile::tempdir().unwrap();
        let plugin_cfg_dir = cfg_dir.path().join("plugins").join("slack");
        std::fs::create_dir_all(&plugin_cfg_dir).unwrap();
        std::fs::write(plugin_cfg_dir.join("00.yaml"), "x: 1\n").unwrap();

        // Register a factory whose closure body is irrelevant —
        // we expect the loader to fail BEFORE it's invoked.
        let mut registry = PluginFactoryRegistry::new();
        let factory: PluginFactory = Box::new(|_m| {
            let err: super::super::factory::BoxError =
                Box::new(std::io::Error::other("factory should not be reached"));
            Err(err)
        });
        registry.register("slack", factory).unwrap();

        let chan_reg = Arc::new(crate::agent::channel_adapter::ChannelAdapterRegistry::new());
        let llm_reg = Arc::new(nexo_llm::LlmRegistry::new());
        let hook_reg = Arc::new(crate::agent::hook_registry::HookRegistry::new());
        let vec_reg = Arc::new(crate::agent::vector_backend_registry::VectorBackendRegistry::new());
        let result = run_plugin_init_loop_with_factory(
            &snap,
            &registry,
            cfg_dir.path(),
            &chan_reg,
            &llm_reg,
            &hook_reg,
            &vec_reg,
            None, // Phase 93.7 bundle (tests don't carry one)
            |_m, _cfg| -> PluginInitContext<'_> {
                unreachable!("ctx_factory must NOT be invoked when config load fails")
            },
        )
        .await;

        // The loader returns SchemaRead Err → InitOutcome::Failed
        // surfaces with "config load: …" prefix; factory closure
        // never ran, so no "factory should not be reached" string
        // in the error.
        match result.outcomes.get("slack") {
            Some(InitOutcome::Failed { error }) => {
                assert!(
                    error.starts_with("config load:"),
                    "expected config-load failure, got {error}"
                );
                assert!(error.contains("missing-schema.json"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(result.handles.is_empty());
    }

    // ── Phase 93.2: configure-before-init helper ─────────────────

    use crate::agent::plugin_host::PluginShutdownError;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Phase 93.2 — minimal NexoPlugin impl that records configure
    /// vs init ordering, holds a programmable configure outcome,
    /// and exposes the captured value via `.last_value()`. Lives
    /// here (not in plugin_host::tests) to keep the test surface
    /// local to the init-loop helper under test.
    struct TestPlugin {
        manifest: PluginManifest,
        init_called: AtomicBool,
        configure_called: AtomicBool,
        last_value: tokio::sync::Mutex<Option<Value>>,
        configure_outcome: Option<Result<(), String>>,
    }

    impl TestPlugin {
        fn new(manifest: PluginManifest) -> Self {
            Self {
                manifest,
                init_called: AtomicBool::new(false),
                configure_called: AtomicBool::new(false),
                last_value: tokio::sync::Mutex::new(None),
                configure_outcome: None,
            }
        }

        fn with_reject(mut self, msg: &str) -> Self {
            self.configure_outcome = Some(Err(msg.to_string()));
            self
        }
    }

    #[async_trait]
    impl NexoPlugin for TestPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        async fn init(
            &self,
            _ctx: &mut PluginInitContext<'_>,
        ) -> Result<(), crate::agent::plugin_host::PluginInitError> {
            self.init_called.store(true, Ordering::SeqCst);
            Ok(())
        }
        async fn shutdown(&self) -> Result<(), PluginShutdownError> {
            Ok(())
        }
        async fn configure(
            &self,
            value: &Value,
        ) -> Result<(), PluginConfigureError> {
            self.configure_called.store(true, Ordering::SeqCst);
            *self.last_value.lock().await = Some(value.clone());
            match &self.configure_outcome {
                None | Some(Ok(())) => Ok(()),
                Some(Err(msg)) => Err(PluginConfigureError::PluginRejected {
                    plugin_id: self.manifest.plugin.id.clone(),
                    source: anyhow::anyhow!(msg.clone()),
                }),
            }
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn manifest_with_schema(id: &str, schema_required_field: Option<&str>) -> PluginManifest {
        let schema = match schema_required_field {
            None => r#"{"type":"object"}"#.to_string(),
            Some(field) => format!(
                r#"{{"type":"object","properties":{{"{field}":{{"type":"string"}}}},"required":["{field}"]}}"#
            ),
        };
        let raw = format!(
            "[plugin]\n\
             id = \"{id}\"\n\
             version = \"0.1.0\"\n\
             name = \"{id}\"\n\
             description = \"fixture\"\n\
             min_nexo_version = \">=0.0.1\"\n\
             \n\
             [plugin.config_schema]\n\
             shape = \"object\"\n\
             schema = '''{schema}'''\n",
        );
        toml::from_str(&raw).expect("manifest TOML parses")
    }

    #[tokio::test]
    async fn configure_helper_calls_plugin_when_schema_valid() {
        let manifest = manifest_with_schema("alpha", None);
        let plugin = Arc::new(TestPlugin::new(manifest.clone()));
        let handle: Arc<dyn NexoPlugin> = plugin.clone();
        let value = Value::Mapping({
            let mut m = serde_yaml::Mapping::new();
            m.insert(Value::String("k".into()), Value::String("v".into()));
            m
        });
        configure_plugin_with_value(&handle, &manifest, &value)
            .await
            .expect("configure ok");
        assert!(plugin.configure_called.load(Ordering::SeqCst));
        let captured = plugin.last_value.lock().await.clone();
        assert_eq!(captured, Some(value));
        // init must NOT have been touched — the helper only handles
        // configure; init is the caller's responsibility.
        assert!(!plugin.init_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn configure_helper_schema_validation_skips_plugin() {
        let manifest = manifest_with_schema("beta", Some("host"));
        let plugin = Arc::new(TestPlugin::new(manifest.clone()));
        let handle: Arc<dyn NexoPlugin> = plugin.clone();
        // Required field "host" missing → schema validation fails.
        let value = Value::Mapping(serde_yaml::Mapping::new());
        let err = configure_plugin_with_value(&handle, &manifest, &value)
            .await
            .expect_err("schema must fail");
        match err {
            PluginConfigureError::SchemaValidation { plugin_id, errors } => {
                assert_eq!(plugin_id, "beta");
                assert!(!errors.is_empty(), "validator should report at least one error");
            }
            other => panic!("expected SchemaValidation, got {other:?}"),
        }
        // Plugin's configure must NEVER run when schema fails.
        assert!(!plugin.configure_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn configure_helper_propagates_plugin_reject() {
        let manifest = manifest_with_schema("gamma", None);
        let plugin = Arc::new(TestPlugin::new(manifest.clone()).with_reject("nope"));
        let handle: Arc<dyn NexoPlugin> = plugin.clone();
        let value = Value::Mapping(serde_yaml::Mapping::new());
        let err = configure_plugin_with_value(&handle, &manifest, &value)
            .await
            .expect_err("plugin rejected");
        match err {
            PluginConfigureError::PluginRejected { plugin_id, .. } => {
                assert_eq!(plugin_id, "gamma");
            }
            other => panic!("expected PluginRejected, got {other:?}"),
        }
        assert!(plugin.configure_called.load(Ordering::SeqCst));
        assert!(!plugin.init_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn configure_helper_skips_validation_when_no_schema() {
        // Backward-compat path: legacy plugin manifest without
        // [plugin.config_schema] — validate_operator_value short
        // -circuits, configure still runs with whatever value the
        // caller supplied.
        let manifest: PluginManifest = toml::from_str(
            "[plugin]\n\
             id = \"legacy\"\n\
             version = \"0.1.0\"\n\
             name = \"legacy\"\n\
             description = \"no schema\"\n\
             min_nexo_version = \">=0.0.1\"\n",
        )
        .unwrap();
        let plugin = Arc::new(TestPlugin::new(manifest.clone()));
        let handle: Arc<dyn NexoPlugin> = plugin.clone();
        let value = Value::Mapping(serde_yaml::Mapping::new());
        configure_plugin_with_value(&handle, &manifest, &value)
            .await
            .expect("no-schema path ok");
        assert!(plugin.configure_called.load(Ordering::SeqCst));
    }

    // ── Phase 93.7: collect_credential_store helper ─────────────

    use nexo_auth::generic_store::GenericCredentialStore;
    use nexo_auth::wire::CredentialsBundle;
    use nexo_auth::handle::TELEGRAM;
    use nexo_auth::CredentialHandle;
    use nexo_auth::resolver::CredentialStores;

    /// Phase 93.7 — minimal credential store impl for init-loop
    /// helper tests. Carries an id + a "version tag" byte so
    /// collision tests can distinguish which store won.
    struct TestGenericStore {
        id: String,
        version: u8,
    }

    #[async_trait]
    impl GenericCredentialStore for TestGenericStore {
        fn plugin_id(&self) -> &str {
            &self.id
        }
        async fn list(&self) -> Vec<String> {
            Vec::new()
        }
        async fn issue(
            &self,
            account_id: &str,
            agent_id: &str,
        ) -> Result<CredentialHandle, nexo_auth::CredentialError> {
            Ok(CredentialHandle::new(TELEGRAM, account_id, agent_id))
        }
        async fn resolve_bytes(
            &self,
            _handle: &CredentialHandle,
        ) -> Result<Vec<u8>, nexo_auth::CredentialError> {
            Ok(vec![self.version])
        }
    }

    /// Phase 93.7 — minimal NexoPlugin that returns a contributed
    /// store from `credential_store()`.
    struct StoreContributingPlugin {
        manifest: PluginManifest,
        store: Arc<dyn GenericCredentialStore>,
    }

    #[async_trait]
    impl NexoPlugin for StoreContributingPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        async fn init(
            &self,
            _ctx: &mut PluginInitContext<'_>,
        ) -> Result<(), crate::agent::plugin_host::PluginInitError> {
            Ok(())
        }
        async fn shutdown(&self) -> Result<(), PluginShutdownError> {
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn credential_store(&self) -> Option<Arc<dyn GenericCredentialStore>> {
            Some(self.store.clone())
        }
    }

    fn empty_bundle() -> CredentialsBundle {
        let resolver = Arc::new(nexo_auth::AgentCredentialResolver::empty());
        CredentialsBundle {
            stores: CredentialStores::empty(),
            resolver,
            breakers: Arc::new(nexo_auth::breaker::BreakerRegistry::default()),
            warnings: Vec::new(),
            stores_v2: dashmap::DashMap::new(),
        }
    }

    fn manifest_for(id: &str) -> PluginManifest {
        let raw = format!(
            "[plugin]\n\
             id = \"{id}\"\n\
             version = \"0.1.0\"\n\
             name = \"{id}\"\n\
             description = \"fixture\"\n\
             min_nexo_version = \">=0.0.1\"\n",
        );
        toml::from_str(&raw).expect("manifest TOML parses")
    }

    #[tokio::test]
    async fn collect_credential_store_inserts_plugin_contribution() {
        let bundle = empty_bundle();
        let plugin = Arc::new(StoreContributingPlugin {
            manifest: manifest_for("alpha"),
            store: Arc::new(TestGenericStore {
                id: "alpha".into(),
                version: 1,
            }),
        });
        let handle: Arc<dyn NexoPlugin> = plugin.clone();
        collect_credential_store(&handle, Some(&bundle));
        assert!(bundle.stores_v2.get("alpha").is_some());
        assert_eq!(bundle.stores_v2.len(), 1);
    }

    #[tokio::test]
    async fn collect_credential_store_collision_last_write_wins() {
        let bundle = empty_bundle();
        let plugin_v1 = Arc::new(StoreContributingPlugin {
            manifest: manifest_for("dup"),
            store: Arc::new(TestGenericStore {
                id: "dup".into(),
                version: 1,
            }),
        });
        let plugin_v2 = Arc::new(StoreContributingPlugin {
            manifest: manifest_for("dup"),
            store: Arc::new(TestGenericStore {
                id: "dup".into(),
                version: 2,
            }),
        });
        let h1: Arc<dyn NexoPlugin> = plugin_v1.clone();
        let h2: Arc<dyn NexoPlugin> = plugin_v2.clone();
        collect_credential_store(&h1, Some(&bundle));
        collect_credential_store(&h2, Some(&bundle));

        let dummy = CredentialHandle::new(TELEGRAM, "x", "y");
        let bytes = bundle
            .stores_v2
            .get("dup")
            .unwrap()
            .resolve_bytes(&dummy)
            .await
            .unwrap();
        assert_eq!(bytes, vec![2u8], "last write wins on collision");
    }

    #[tokio::test]
    async fn collect_credential_store_skips_when_bundle_none() {
        // Non-factory init path passes `None`; helper short-circuits.
        let plugin = Arc::new(StoreContributingPlugin {
            manifest: manifest_for("beta"),
            store: Arc::new(TestGenericStore {
                id: "beta".into(),
                version: 1,
            }),
        });
        let handle: Arc<dyn NexoPlugin> = plugin.clone();
        // No assertion needed — function is no-op + must not panic.
        collect_credential_store(&handle, None);
    }
}
