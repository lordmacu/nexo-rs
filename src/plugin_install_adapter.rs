//! Phase 97.1 — admin RPC adapter for `nexo/admin/plugins/{scan,install,uninstall}`.
//!
//! Wraps the existing `nexo plugin install` CLI pipeline
//! ([`crate::plugin_install::install_plugin_silent`]) plus a
//! `cargo install` fallback so the admin UI can drive the plugin
//! lifecycle without operator terminal access. The CLI keeps its
//! original UX — this adapter just exposes the same orchestration
//! via the admin RPC dispatcher.
//!
//! Scope of this iteration:
//!   - `install` for `InstallSource::Release` → GitHub release
//!     download + sha256 + cosign + extract via the CLI's silent
//!     variant. Returns the typed report.
//!   - `install` for `InstallSource::Cargo` → `cargo install
//!     <crate> [--version <v>] [--force]` subprocess. Captures
//!     stdout / stderr (clipped to 16 KiB).
//!   - `scan` and `uninstall` return a typed deferred error
//!     directing the operator to restart the daemon for the live
//!     activation. Hot-spawn integration ships in Phase 97.1.β —
//!     extracted as a separate task because it requires
//!     constructing the per-plugin `PluginInitContext` (channel
//!     adapter registry, hook registry, llm registry, etc.) from
//!     this adapter — they currently live as captures in main.rs's
//!     boot path and a refactor is needed to share them.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::agent::admin_rpc::domains::plugin_install::PluginInstaller;
use nexo_core::agent::nexo_plugin_registry::{
    discover, run_plugin_init_loop_with_factory, NexoPluginRegistrySnapshot, PluginDiscoveryConfig,
    PluginFactoryRegistry, SubprocessCtxStubs, SubprocessRuntime,
};
use nexo_setup::hot_spawn::{
    hot_add_plugin_metrics, hot_add_plugins_to_snapshot, hot_rebuild_skills_from_snapshot,
    hot_remove_plugin_from_snapshot, hot_remove_plugin_metrics, register_plugin_in_live_runtime,
    unregister_plugin_from_live_runtime, HotPluginCtx,
};
use nexo_tool_meta::admin::plugin_install::{
    InstallSource, PluginsInstallParams, PluginsInstallResponse, PluginsScanResponse,
    PluginsSetEnabledParams, PluginsSetEnabledResponse, PluginsUninstallParams,
    PluginsUninstallResponse,
};

use crate::plugin_install::install_plugin_silent;

/// Cap on cargo output sent back over admin RPC. Cargo prints
/// every dependency compile line on `--verbose` — without a clip
/// the JSON reply balloons to multi-MB and the admin UI struggles
/// to render.
const CARGO_OUTPUT_CLIP: usize = 16 * 1024;

/// Late-bind wrapper used by the admin RPC dispatcher (Phase 97.B
/// pattern). Constructed empty at admin_bootstrap; main.rs fills the
/// cell AFTER `wire_plugin_registry` returns. Dispatcher calls
/// arriving before the cell is populated get a clean boot-window
/// error that the UI can render as a transient toast.
#[derive(Debug, Clone)]
pub struct LivePluginInstallerCell {
    inner: Arc<tokio::sync::OnceCell<Arc<LivePluginInstaller>>>,
}

impl LivePluginInstallerCell {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    pub fn set(&self, installer: Arc<LivePluginInstaller>) -> Result<(), &'static str> {
        self.inner.set(installer).map_err(|_| "already populated")
    }
}

impl Default for LivePluginInstallerCell {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PluginInstaller for LivePluginInstallerCell {
    async fn scan(&self) -> anyhow::Result<PluginsScanResponse> {
        match self.inner.get() {
            Some(i) => i.scan().await,
            None => {
                nexo_core::telemetry::inc_plugin_install_boot_race("scan", "cell_unset");
                anyhow::bail!("plugin installer not yet populated (daemon still booting)")
            }
        }
    }
    async fn install(
        &self,
        params: &PluginsInstallParams,
    ) -> anyhow::Result<PluginsInstallResponse> {
        match self.inner.get() {
            Some(i) => i.install(params).await,
            None => {
                nexo_core::telemetry::inc_plugin_install_boot_race("install", "cell_unset");
                anyhow::bail!("plugin installer not yet populated (daemon still booting)")
            }
        }
    }
    async fn uninstall(
        &self,
        params: &PluginsUninstallParams,
    ) -> anyhow::Result<PluginsUninstallResponse> {
        match self.inner.get() {
            Some(i) => i.uninstall(params).await,
            None => {
                nexo_core::telemetry::inc_plugin_install_boot_race("uninstall", "cell_unset");
                anyhow::bail!("plugin installer not yet populated (daemon still booting)")
            }
        }
    }
    async fn set_enabled(
        &self,
        params: &PluginsSetEnabledParams,
    ) -> anyhow::Result<PluginsSetEnabledResponse> {
        match self.inner.get() {
            Some(i) => i.set_enabled(params).await,
            None => {
                nexo_core::telemetry::inc_plugin_install_boot_race("set_enabled", "cell_unset");
                anyhow::bail!("plugin installer not yet populated (daemon still booting)")
            }
        }
    }
}

/// Phase 97.1.β — fixtures required to spawn a freshly-installed
/// plugin into the live runtime. main.rs builds this AFTER
/// `wire_plugin_registry_with_runtime` returns + threads it into
/// the installer via [`LivePluginInstaller::with_hot_install`].
/// Every field is `Arc`-cloned from the boot path so a hot-spawn
/// sees the same registries the boot-time plugins did — the
/// channel adapter / hook / vector backend / tool registries are
/// shared, so new plugins' inbound traffic + tool registrations
/// land in the same maps the live runtime is already reading.
pub struct HotInstallCtx {
    pub factory_registry: Arc<PluginFactoryRegistry>,
    pub subprocess_runtime: SubprocessRuntime,
    pub channel_adapter_registry: Arc<nexo_core::agent::channel_adapter::ChannelAdapterRegistry>,
    pub llm_registry: Arc<nexo_llm::LlmRegistry>,
    pub hook_registry: Arc<nexo_core::agent::hook_registry::HookRegistry>,
    pub vector_backend_registry:
        Arc<nexo_core::agent::vector_backend_registry::VectorBackendRegistry>,
    pub tool_registry: Arc<nexo_core::agent::tool_registry::ToolRegistry>,
    pub discovery_cfg: PluginDiscoveryConfig,
    pub current_version: semver::Version,
    pub hot_plugin_ctx: HotPluginCtx,
}

impl std::fmt::Debug for HotInstallCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotInstallCtx")
            .field("current_version", &self.current_version.to_string())
            .field(
                "config_dir",
                &self.subprocess_runtime.config_dir.display().to_string(),
            )
            .field("factories", &self.factory_registry.len())
            .finish()
    }
}

/// The real adapter. Holds the daemon's `config_dir` for the
/// silent-install pipeline + (after `with_hot_install`) the runtime
/// fixtures needed to spawn freshly-installed plugins without a
/// daemon restart.
pub struct LivePluginInstaller {
    config_dir: PathBuf,
    /// Phase 97.1.β — `None` until main.rs threads the post-wire
    /// runtime fixtures via [`Self::with_hot_install`]. Calls to
    /// `scan` / `uninstall` while `None` return a typed
    /// "still booting" error.
    hot_install: Option<HotInstallCtx>,
}

impl std::fmt::Debug for LivePluginInstaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LivePluginInstaller")
            .field("config_dir", &self.config_dir.display().to_string())
            .field("hot_install", &self.hot_install.is_some())
            .finish()
    }
}

impl LivePluginInstaller {
    pub fn new(config_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            config_dir,
            hot_install: None,
        })
    }

    /// Phase 97.1.β — install the runtime hot-spawn fixtures. Used
    /// once at main.rs post-wire. Returns a fresh `Arc<Self>` so the
    /// caller can swap the OnceCell entry safely.
    pub fn with_hot_install(self: Arc<Self>, ctx: HotInstallCtx) -> Arc<Self> {
        Arc::new(Self {
            config_dir: self.config_dir.clone(),
            hot_install: Some(ctx),
        })
    }

    async fn install_release(
        &self,
        params: &PluginsInstallParams,
    ) -> anyhow::Result<PluginsInstallResponse> {
        // `repo` defaults to lordmacu/<crate_name> per the
        // convention for first-party Nexo plugins. Operators can
        // override for forks.
        let repo = params
            .repo
            .clone()
            .unwrap_or_else(|| format!("lordmacu/{}", params.crate_name));
        let tag = match params.version.as_deref() {
            Some(v) => {
                // The CLI accepts both `v0.1.0` and `0.1.0`; pass
                // through unchanged so semver pins without `v` work.
                v.to_string()
            }
            None => "latest".to_string(),
        };
        let coords = format!("{repo}@{tag}");

        // Phase 97.1 — admin RPC always passes None for
        // dest_override + target_override so the CLI's standard
        // resolution applies (config search_paths[0] → state-dir
        // fallback, compile-time TARGET).
        // Phase 98.4 — trust flags forwarded from params (parity with CLI).
        let trust_enforcement = if params.skip_signature_verify {
            "user_skip"
        } else if params.require_signature {
            "user_require"
        } else {
            "policy_applied"
        }
        .to_string();
        match install_plugin_silent(
            &self.config_dir,
            &coords,
            None,
            None,
            params.require_signature,
            params.skip_signature_verify,
        )
        .await
        {
            Ok(report) => Ok(PluginsInstallResponse {
                crate_name: params.crate_name.clone(),
                installed_version: Some(report.version),
                spawned: Vec::new(), // populated by 97.1.β scan integration
                cargo_stdout: String::new(),
                cargo_stderr: String::new(),
                trust_enforcement,
                signature_verified: report.signature_verified,
                signature_identity: report.signature_identity,
                signature_issuer: report.signature_issuer,
                trust_mode: report.trust_mode.to_string(),
                trust_policy_matched: report.trust_policy_matched,
            }),
            Err(err_report) => {
                anyhow::bail!(
                    "{} ({}){}",
                    err_report.error,
                    err_report.kind,
                    err_report
                        .available
                        .as_ref()
                        .map(|av| format!("; available targets: {}", av.join(", ")))
                        .unwrap_or_default()
                )
            }
        }
    }

    async fn install_cargo(
        &self,
        params: &PluginsInstallParams,
    ) -> anyhow::Result<PluginsInstallResponse> {
        // Build argv. Validated upstream by the handler's char
        // class checks, so direct interpolation is safe.
        let mut cmd = tokio::process::Command::new("cargo");
        cmd.arg("install").arg(&params.crate_name);
        if let Some(v) = params.version.as_deref() {
            cmd.arg("--version").arg(v);
        }
        if params.force {
            cmd.arg("--force");
        }
        // Locked is the default we want for reproducibility; mirror
        // the install.sh defaults.
        cmd.arg("--locked");

        let out = cmd
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("cargo install spawn failed: {e}"))?;

        let stdout = clip_utf8(&out.stdout, CARGO_OUTPUT_CLIP);
        let stderr = clip_utf8(&out.stderr, CARGO_OUTPUT_CLIP);

        if !out.status.success() {
            anyhow::bail!(
                "cargo install exited {} — stderr tail: {}",
                out.status.code().unwrap_or(-1),
                stderr
                    .lines()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }

        // Best-effort version parse from cargo output. Pattern:
        // `Installed package \`nexo-plugin-X v0.3.0 (registry+...)\``.
        let installed_version = parse_cargo_installed_version(&stdout)
            .or_else(|| parse_cargo_installed_version(&stderr))
            .or_else(|| params.version.clone());

        Ok(PluginsInstallResponse {
            crate_name: params.crate_name.clone(),
            installed_version,
            spawned: Vec::new(), // populated by 97.1.β scan integration
            cargo_stdout: stdout,
            cargo_stderr: stderr,
            // Phase 98.4 — Cargo path bypasses signature verification.
            // No cosign cert to validate; trust deferred.
            trust_enforcement: "cargo_skipped".to_string(),
            signature_verified: false,
            signature_identity: None,
            signature_issuer: None,
            trust_mode: String::new(),
            trust_policy_matched: None,
        })
    }
}

#[async_trait]
impl PluginInstaller for LivePluginInstaller {
    /// Phase 97.1.β — re-run the discovery walker, diff against the
    /// live `plugin_handles_cell`, and hot-spawn every plugin that's
    /// on disk but not yet running. Stale handles (registered but
    /// no longer on disk) surface in the response without being
    /// auto-removed — that's `uninstall`'s job.
    async fn scan(&self) -> anyhow::Result<PluginsScanResponse> {
        let ctx = self.hot_install.as_ref().ok_or_else(|| {
            nexo_core::telemetry::inc_plugin_install_boot_race("scan", "hot_install_unset");
            anyhow::anyhow!(
                "scan unavailable: hot-install fixtures not yet populated \
                 (daemon still booting). Retry in ~1 second."
            )
        })?;

        // 1. Discover.
        let snap = discover(&ctx.discovery_cfg, &ctx.current_version);
        let mut warnings: Vec<String> = snap
            .last_report
            .diagnostics
            .iter()
            .filter(|d| {
                matches!(
                    d.level,
                    nexo_core::agent::nexo_plugin_registry::DiagnosticLevel::Warn
                        | nexo_core::agent::nexo_plugin_registry::DiagnosticLevel::Error
                )
            })
            .map(|d| format!("{}: {:?}", d.path.display(), d.kind))
            .collect();

        // 2. Diff against live cell.
        let existing_ids: std::collections::BTreeSet<String> = {
            let guard = ctx.hot_plugin_ctx.plugin_handles.read().await;
            guard
                .as_ref()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default()
        };
        let new_plugins: Vec<_> = snap
            .plugins
            .iter()
            .filter(|p| !existing_ids.contains(&p.manifest.plugin.id))
            .cloned()
            .collect();
        let discovered_ids: std::collections::BTreeSet<String> = snap
            .plugins
            .iter()
            .map(|p| p.manifest.plugin.id.clone())
            .collect();
        let stale: Vec<String> = existing_ids
            .iter()
            .filter(|id| !discovered_ids.contains(*id))
            .cloned()
            .collect();

        if new_plugins.is_empty() {
            return Ok(PluginsScanResponse {
                spawned: Vec::new(),
                stale,
                warnings,
            });
        }

        // 3. Build a filtered snapshot covering ONLY the new plugins
        // so the init loop doesn't redundantly re-init the
        // already-live ones (it would attempt a second
        // subprocess spawn for the same channel, which the
        // upstream server kicks).
        let filtered_snap = {
            let mut owned: NexoPluginRegistrySnapshot = (*snap).clone();
            owned.plugins = new_plugins;
            owned
        };

        // 4. Build context stubs that reuse the SAME shared
        // registries the boot path uses. Crucial — any new plugin's
        // remote channel adapter / hook registration must land in
        // the same maps the live runtime is already reading.
        let stubs = SubprocessCtxStubs::build_with_shared_registries(
            &ctx.subprocess_runtime,
            ctx.channel_adapter_registry.clone(),
            ctx.hook_registry.clone(),
            ctx.vector_backend_registry.clone(),
            ctx.tool_registry.clone(),
        );

        // 5. Drive the init loop on the filtered snapshot.
        let result = run_plugin_init_loop_with_factory(
            &filtered_snap,
            &ctx.factory_registry,
            ctx.subprocess_runtime.config_dir.as_path(),
            &ctx.channel_adapter_registry,
            &ctx.llm_registry,
            &ctx.hook_registry,
            &ctx.vector_backend_registry,
            None, // Phase 93.7 — bundle threading still deferred.
            |manifest, plugin_cfg| {
                stubs.context_for_plugin(manifest, &ctx.subprocess_runtime, plugin_cfg)
            },
        )
        .await;

        // 6. Capture init failures + register the successful handles
        // into the live runtime via the hot-spawn helper.
        for (id, outcome) in result.outcomes.iter() {
            if let nexo_core::agent::nexo_plugin_registry::InitOutcome::Failed { error } = outcome {
                warnings.push(format!("{id}: init failed — {error}"));
            }
        }

        let mut spawned = Vec::new();
        for (id, handle) in result.handles.into_iter() {
            match register_plugin_in_live_runtime(&id, handle, &ctx.hot_plugin_ctx).await {
                Ok(()) => {
                    // Phase 97.1.γ — hot-add the plugin's metrics descriptor
                    // (skills are rebuilt from the snapshot below).
                    if let Some(dp) = filtered_snap
                        .plugins
                        .iter()
                        .find(|p| p.manifest.plugin.id == id)
                    {
                        hot_add_plugin_metrics(
                            &ctx.hot_plugin_ctx.plugin_metrics,
                            &id,
                            &dp.manifest,
                        );
                    }
                    spawned.push(id);
                }
                Err(e) => {
                    warnings.push(format!("{id}: register_in_runtime failed — {e}"));
                }
            }
        }

        // Phase 97.1.γ 3a — add the new plugins to the live registry
        // snapshot so admin-UI (Phase 99 `plugin_ui/list`), discovery,
        // and capability views see them without a restart.
        hot_add_plugins_to_snapshot(&ctx.hot_plugin_ctx.registry, &filtered_snap.plugins);
        // #1 — rebuild skills from the updated snapshot (SSOT; correct
        // first-plugin-wins + conflict re-promotion).
        hot_rebuild_skills_from_snapshot(
            &ctx.hot_plugin_ctx.plugin_skills,
            &ctx.hot_plugin_ctx.registry,
        );

        tracing::info!(
            spawned_count = spawned.len(),
            stale_count = stale.len(),
            warnings_count = warnings.len(),
            "plugins/scan: hot-spawn cycle complete"
        );

        Ok(PluginsScanResponse {
            spawned,
            stale,
            warnings,
        })
    }

    async fn install(
        &self,
        params: &PluginsInstallParams,
    ) -> anyhow::Result<PluginsInstallResponse> {
        let mut response = match params.source {
            InstallSource::Release => self.install_release(params).await?,
            InstallSource::Cargo => self.install_cargo(params).await?,
        };

        // Phase 97.1.β — auto-scan after a successful install so the
        // new plugin is live immediately. Failure here is logged but
        // doesn't fail the install RPC — the binary is on disk and
        // a manual `plugins/scan` (or daemon restart) recovers.
        match self.scan().await {
            Ok(scan_resp) => {
                response.spawned = scan_resp.spawned;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "post-install scan failed; plugin installed but not yet live. \
                     Operator can retry via `nexo/admin/plugins/scan`."
                );
            }
        }

        Ok(response)
    }

    /// Phase 97.1.β — stop the subprocess, drop the handle, then
    /// optionally `cargo uninstall`. Idempotent — missing plugins
    /// report `removed: false`.
    async fn uninstall(
        &self,
        params: &PluginsUninstallParams,
    ) -> anyhow::Result<PluginsUninstallResponse> {
        let ctx = self.hot_install.as_ref().ok_or_else(|| {
            nexo_core::telemetry::inc_plugin_install_boot_race("uninstall", "hot_install_unset");
            anyhow::anyhow!(
                "uninstall unavailable: hot-install fixtures not yet populated \
                 (daemon still booting). Retry in ~1 second."
            )
        })?;

        // 1. Drop the handle from the shared cell. This drops the
        // last Arc reference to the subprocess plugin; the
        // `kill_on_drop(true)` child guard tears the subprocess
        // down on the next runtime tick.
        let removed =
            unregister_plugin_from_live_runtime(&params.plugin_id, &ctx.hot_plugin_ctx).await?;
        // Phase 97.1.γ — drop the plugin's metrics + remove it from the
        // live registry snapshot (admin-UI / discovery), then rebuild
        // skills from the updated snapshot (#1 — re-promotes a runner-up
        // if this plugin had won a skill-name conflict).
        hot_remove_plugin_metrics(&ctx.hot_plugin_ctx.plugin_metrics, &params.plugin_id);
        hot_remove_plugin_from_snapshot(&ctx.hot_plugin_ctx.registry, &params.plugin_id);
        hot_rebuild_skills_from_snapshot(
            &ctx.hot_plugin_ctx.plugin_skills,
            &ctx.hot_plugin_ctx.registry,
        );

        // 2. Optional `cargo uninstall`. Only fires when explicitly
        // requested; the binary may have come from a Release tarball
        // (different code path) so cargo would error with
        // "package not installed" — that's an informational warning
        // path, not a failure of the in-memory removal.
        let (cargo_uninstalled, cargo_stdout, cargo_stderr) = if params.cargo_uninstall {
            let crate_name = params
                .crate_name
                .clone()
                .unwrap_or_else(|| params.plugin_id.clone());
            let out = tokio::process::Command::new("cargo")
                .arg("uninstall")
                .arg(&crate_name)
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("cargo uninstall spawn failed: {e}"))?;
            let stdout = clip_utf8(&out.stdout, CARGO_OUTPUT_CLIP);
            let stderr = clip_utf8(&out.stderr, CARGO_OUTPUT_CLIP);
            (out.status.success(), stdout, stderr)
        } else {
            (false, String::new(), String::new())
        };

        Ok(PluginsUninstallResponse {
            plugin_id: params.plugin_id.clone(),
            removed,
            cargo_uninstalled,
            cargo_stdout,
            cargo_stderr,
        })
    }

    /// Phase 98 follow-up — toggle a plugin's enabled state.
    ///
    /// `enabled = false` (disable): append the id to
    /// `plugins/discovery.yaml`'s `disabled[]` then hot-remove the
    /// live handle. The on-disk binary stays put.
    ///
    /// `enabled = true` (enable): remove the id from `disabled[]`
    /// then hot-spawn it. The spawn uses a discovery cfg CLONE with
    /// the id stripped from `disabled` — `ctx.discovery_cfg` is a
    /// boot-time snapshot that still lists the id, so a plain
    /// `discover()` would skip it (same staleness Phase 97.3 solved
    /// for agent spawn).
    async fn set_enabled(
        &self,
        params: &PluginsSetEnabledParams,
    ) -> anyhow::Result<PluginsSetEnabledResponse> {
        let ctx = self.hot_install.as_ref().ok_or_else(|| {
            nexo_core::telemetry::inc_plugin_install_boot_race("set_enabled", "hot_install_unset");
            anyhow::anyhow!(
                "set_enabled unavailable: hot-install fixtures not yet populated \
                 (daemon still booting). Retry in ~1 second."
            )
        })?;

        // 1. Persist the toggle in discovery.yaml. Survives restart.
        let config_changed = nexo_setup::plugin_enabled::set_plugin_disabled(
            &self.config_dir,
            &params.plugin_id,
            !params.enabled,
        )?;

        let mut warnings: Vec<String> = Vec::new();

        if !params.enabled {
            // ── Disable: drop the live handle (binary stays). ──
            let removed =
                unregister_plugin_from_live_runtime(&params.plugin_id, &ctx.hot_plugin_ctx).await?;
            // Phase 97.1.γ — disabling drops its metrics + removes it
            // from the snapshot, then rebuilds skills from the snapshot.
            hot_remove_plugin_metrics(&ctx.hot_plugin_ctx.plugin_metrics, &params.plugin_id);
            hot_remove_plugin_from_snapshot(&ctx.hot_plugin_ctx.registry, &params.plugin_id);
            hot_rebuild_skills_from_snapshot(
                &ctx.hot_plugin_ctx.plugin_skills,
                &ctx.hot_plugin_ctx.registry,
            );
            tracing::info!(
                plugin_id = %params.plugin_id,
                config_changed,
                removed,
                "plugins/set_enabled: disabled"
            );
            return Ok(PluginsSetEnabledResponse {
                plugin_id: params.plugin_id.clone(),
                enabled: false,
                config_changed,
                spawned: Vec::new(),
                removed,
                warnings,
            });
        }

        // ── Enable: discover with a cfg clone that no longer lists
        // this id in `disabled`, filter to just this plugin, spawn. ──
        let mut fresh_cfg: PluginDiscoveryConfig = ctx.discovery_cfg.clone();
        fresh_cfg.disabled.retain(|d| d != &params.plugin_id);

        let snap = discover(&fresh_cfg, &ctx.current_version);

        // Already live? (operator double-clicked enable). No-op spawn.
        let already_live = {
            let guard = ctx.hot_plugin_ctx.plugin_handles.read().await;
            guard
                .as_ref()
                .map(|m| m.contains_key(&params.plugin_id))
                .unwrap_or(false)
        };

        let target: Vec<_> = snap
            .plugins
            .iter()
            .filter(|p| p.manifest.plugin.id == params.plugin_id)
            .cloned()
            .collect();

        if target.is_empty() {
            warnings.push(format!(
                "{}: removed from disabled[] but no manifest discoverable on disk \
                 (binary missing? run install). Config saved; nothing to spawn.",
                params.plugin_id
            ));
            return Ok(PluginsSetEnabledResponse {
                plugin_id: params.plugin_id.clone(),
                enabled: true,
                config_changed,
                spawned: Vec::new(),
                removed: false,
                warnings,
            });
        }

        if already_live {
            return Ok(PluginsSetEnabledResponse {
                plugin_id: params.plugin_id.clone(),
                enabled: true,
                config_changed,
                spawned: Vec::new(),
                removed: false,
                warnings,
            });
        }

        let filtered_snap = {
            let mut owned: NexoPluginRegistrySnapshot = (*snap).clone();
            owned.plugins = target;
            owned
        };

        let stubs = SubprocessCtxStubs::build_with_shared_registries(
            &ctx.subprocess_runtime,
            ctx.channel_adapter_registry.clone(),
            ctx.hook_registry.clone(),
            ctx.vector_backend_registry.clone(),
            ctx.tool_registry.clone(),
        );

        let result = run_plugin_init_loop_with_factory(
            &filtered_snap,
            &ctx.factory_registry,
            ctx.subprocess_runtime.config_dir.as_path(),
            &ctx.channel_adapter_registry,
            &ctx.llm_registry,
            &ctx.hook_registry,
            &ctx.vector_backend_registry,
            None,
            |manifest, plugin_cfg| {
                stubs.context_for_plugin(manifest, &ctx.subprocess_runtime, plugin_cfg)
            },
        )
        .await;

        for (id, outcome) in result.outcomes.iter() {
            if let nexo_core::agent::nexo_plugin_registry::InitOutcome::Failed { error } = outcome {
                warnings.push(format!("{id}: init failed — {error}"));
            }
        }

        let mut spawned = Vec::new();
        for (id, handle) in result.handles.into_iter() {
            match register_plugin_in_live_runtime(&id, handle, &ctx.hot_plugin_ctx).await {
                Ok(()) => {
                    // Phase 97.1.γ — enabling a plugin surfaces its metrics
                    // live (skills rebuilt from the snapshot below).
                    if let Some(dp) = filtered_snap
                        .plugins
                        .iter()
                        .find(|p| p.manifest.plugin.id == id)
                    {
                        hot_add_plugin_metrics(
                            &ctx.hot_plugin_ctx.plugin_metrics,
                            &id,
                            &dp.manifest,
                        );
                    }
                    spawned.push(id);
                }
                Err(e) => warnings.push(format!("{id}: register_in_runtime failed — {e}")),
            }
        }

        // Phase 97.1.γ 3a — surface the enabled plugin in the live
        // registry snapshot (admin-UI / discovery) + rebuild skills (#1).
        hot_add_plugins_to_snapshot(&ctx.hot_plugin_ctx.registry, &filtered_snap.plugins);
        hot_rebuild_skills_from_snapshot(
            &ctx.hot_plugin_ctx.plugin_skills,
            &ctx.hot_plugin_ctx.registry,
        );

        tracing::info!(
            plugin_id = %params.plugin_id,
            config_changed,
            spawned_count = spawned.len(),
            warnings_count = warnings.len(),
            "plugins/set_enabled: enabled"
        );

        Ok(PluginsSetEnabledResponse {
            plugin_id: params.plugin_id.clone(),
            enabled: true,
            config_changed,
            spawned,
            removed: false,
            warnings,
        })
    }
}

// ── helpers ─────────────────────────────────────────────────────

fn clip_utf8(raw: &[u8], max: usize) -> String {
    if raw.len() <= max {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let tail = &raw[raw.len().saturating_sub(max)..];
    let mut out = String::from("…");
    out.push_str(&String::from_utf8_lossy(tail));
    out
}

fn parse_cargo_installed_version(text: &str) -> Option<String> {
    // `Installed package \`nexo-plugin-X v0.3.0 …\``
    for line in text.lines() {
        let line = line.trim_start();
        if let Some(after) = line.strip_prefix("Installed package `") {
            for token in after.split_whitespace() {
                if let Some(v) = token.strip_prefix('v') {
                    // Take chars up to whitespace or `(`. Already
                    // token-split so just trim trailing punctuation.
                    let v = v.trim_end_matches([',', ')', '`']);
                    if v.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+')
                    {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cargo_version_picks_v_prefix_token() {
        let s = "    Updating crates.io index\n\
                 Installed package `nexo-plugin-telegram v0.3.0 (registry+https://github.com/rust-lang/crates.io-index)` (executable `nexo-plugin-telegram`)";
        assert_eq!(parse_cargo_installed_version(s), Some("0.3.0".into()));
    }

    #[test]
    fn parse_cargo_version_rejects_unrelated_lines() {
        assert!(parse_cargo_installed_version("Compiling nexo-plugin-X v0.3.0").is_none());
    }

    #[test]
    fn clip_utf8_returns_full_when_under_limit() {
        let raw = b"short";
        assert_eq!(clip_utf8(raw, 100), "short");
    }

    #[test]
    fn clip_utf8_keeps_tail_with_prefix_marker() {
        let raw = vec![b'a'; 100];
        let out = clip_utf8(&raw, 10);
        assert!(out.starts_with('…'));
        assert_eq!(out.chars().count(), 11);
    }

    #[tokio::test]
    async fn cell_returns_boot_window_error_when_unset() {
        let cell = LivePluginInstallerCell::new();
        let res = cell.scan().await;
        assert!(res.unwrap_err().to_string().contains("still booting"));
    }

    // Phase 98.2 — boot-window race metric.
    //
    // These tests share the global `nexo_core::telemetry` registry,
    // which is process-wide. The telemetry module's own tests
    // serialize via TEST_LOCK + reset_for_test(); we mirror that here
    // by snapshotting + comparing deltas rather than absolute values
    // so parallel test runs don't trip each other.

    /// Returns the current value of
    /// `plugin_install_boot_race_total{method,phase}` parsed from the
    /// prometheus render output, or 0 if absent.
    fn current_boot_race_count(method: &str, phase: &str) -> u64 {
        let body = nexo_core::telemetry::render_prometheus(false);
        let needle =
            format!("plugin_install_boot_race_total{{method=\"{method}\",phase=\"{phase}\"}} ");
        for line in body.lines() {
            if let Some(rest) = line.strip_prefix(&needle) {
                return rest.trim().parse::<u64>().unwrap_or(0);
            }
        }
        0
    }

    #[tokio::test]
    async fn cell_unset_bail_increments_counter_per_method() {
        let cell = LivePluginInstallerCell::new();

        let scan_before = current_boot_race_count("scan", "cell_unset");
        let install_before = current_boot_race_count("install", "cell_unset");
        let uninstall_before = current_boot_race_count("uninstall", "cell_unset");

        let _ = cell.scan().await;
        let _ = cell
            .install(&PluginsInstallParams {
                crate_name: "x".into(),
                version: None,
                repo: None,
                source: InstallSource::Release,
                force: false,
                require_signature: false,
                skip_signature_verify: false,
            })
            .await;
        let _ = cell
            .uninstall(&PluginsUninstallParams {
                plugin_id: "x".into(),
                cargo_uninstall: false,
                crate_name: None,
            })
            .await;

        assert_eq!(
            current_boot_race_count("scan", "cell_unset"),
            scan_before + 1,
            "scan bail did not increment cell_unset counter"
        );
        assert_eq!(
            current_boot_race_count("install", "cell_unset"),
            install_before + 1,
            "install bail did not increment cell_unset counter"
        );
        assert_eq!(
            current_boot_race_count("uninstall", "cell_unset"),
            uninstall_before + 1,
            "uninstall bail did not increment cell_unset counter"
        );
    }

    #[tokio::test]
    async fn hot_install_unset_bail_increments_counter_for_scan_and_uninstall() {
        // Constructed without `with_hot_install` so `scan` + `uninstall`
        // hit the inner `hot_install.as_ref()` bail. `install_release` /
        // `install_cargo` don't gate on `hot_install`, so we don't
        // exercise install here.
        let installer = LivePluginInstaller::new(std::env::temp_dir());

        let scan_before = current_boot_race_count("scan", "hot_install_unset");
        let uninstall_before = current_boot_race_count("uninstall", "hot_install_unset");

        let _ = installer.scan().await;
        let _ = installer
            .uninstall(&PluginsUninstallParams {
                plugin_id: "x".into(),
                cargo_uninstall: false,
                crate_name: None,
            })
            .await;

        assert_eq!(
            current_boot_race_count("scan", "hot_install_unset"),
            scan_before + 1,
            "scan bail did not increment hot_install_unset counter"
        );
        assert_eq!(
            current_boot_race_count("uninstall", "hot_install_unset"),
            uninstall_before + 1,
            "uninstall bail did not increment hot_install_unset counter"
        );
    }
}
