//! [`LspManager`]: top-level handle the agent runtime keeps alive.
//!
//! Owns the launcher (boot-time probe), the per-`(workspace,
//! language)` session map, the idle reaper task, and the `execute`
//! entry-point that the [`crate::session::LspSession`] machinery
//! plugs into.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/services/lsp/LSPServerManager.ts:59-200`
//!     — extension map + `getServerForFile`, init-loop, shutdown
//!     `Promise.allSettled`. We mirror with `try_join_all` /
//!     `join_all` over `Arc<LspSession>`s.
//!   * `claude-code-leak/src/services/lsp/manager.ts:100-208` —
//!     `isLspConnected()` gating + initialization state machine.
//!     We collapse into a single `LspManager` rather than the
//!     leak's mutable global because we run multiple instances per
//!     test and Phase 18 hot-reload needs to swap the manager
//!     atomically.

use crate::client::FileDiagnostics;
use crate::error::LspError;
use crate::format::{
    build_output, format_diagnostics, format_hover, format_location, format_locations,
    format_workspace_symbols, resolve_input_path,
};
use crate::launcher::LspLauncher;
use crate::session::{session_from_launcher, LspSession, SessionConfig};
use crate::types::{
    language_for_extension, LspLanguage, LspRequest, LspToolOutput, ResolvedCapabilities,
    ServerStatus, SessionState,
};
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Per-binding policy snapshot the manager needs at call time. The
/// caller (`nexo-core::agent::lsp_tool`) constructs this from the
/// binding's `LspPolicy` (Phase 79.5 step 9) so that this crate
/// stays free of `nexo-config` dep.
#[derive(Debug, Clone, Default)]
pub struct ExecutePolicy {
    /// Empty = all discovered languages allowed.
    pub allowed_languages: Vec<LspLanguage>,
}

impl ExecutePolicy {
    pub fn permits(&self, lang: LspLanguage) -> bool {
        self.allowed_languages.is_empty() || self.allowed_languages.contains(&lang)
    }
}

pub struct LspManager {
    launcher: LspLauncher,
    sessions: DashMap<(PathBuf, LspLanguage), Arc<LspSession>>,
    config: SessionConfig,
    cancel: CancellationToken,
    idle_reaper: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl LspManager {
    /// Build a manager with a freshly-probed launcher. Spawns the
    /// idle reaper task in the background.
    pub fn new(config: SessionConfig) -> Arc<Self> {
        Self::with_launcher(LspLauncher::probe(), config)
    }

    /// Test entry point — pass an explicit launcher (typically built
    /// with `LspLauncher::probe_with`).
    pub fn with_launcher(launcher: LspLauncher, config: SessionConfig) -> Arc<Self> {
        let cancel = CancellationToken::new();
        let manager = Arc::new(Self {
            launcher,
            sessions: DashMap::new(),
            config,
            cancel: cancel.clone(),
            idle_reaper: tokio::sync::Mutex::new(None),
        });
        let weak = Arc::downgrade(&manager);
        let idle_teardown = manager.config.idle_teardown;
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            idle_reaper_loop(weak, idle_teardown, cancel_clone).await;
        });
        // Best-effort store of the handle for explicit shutdown.
        if let Ok(mut guard) = manager.idle_reaper.try_lock() {
            *guard = Some(handle);
        }
        manager
    }

    /// Probe-time discovery snapshot for diagnostics.
    pub fn discovered_languages(&self) -> Vec<LspLanguage> {
        self.launcher.discovered_languages()
    }

    /// Whether the launcher found at least one binary on PATH.
    pub fn any_available(&self) -> bool {
        self.launcher.any_available()
    }

    /// `OR`-fold of every running session's capabilities. Drives
    /// the dynamic `Lsp` tool description so the model never sees
    /// kinds backed only by a missing or stopped server.
    pub async fn aggregated_capabilities(&self) -> ResolvedCapabilities {
        let mut out = ResolvedCapabilities::default();
        for s in self.sessions.iter() {
            let caps = s.value().capabilities().await;
            out = out.or(caps);
        }
        out
    }

    /// Operator-facing snapshot for `nexo setup doctor` / admin-ui.
    pub async fn server_status(&self) -> Vec<ServerStatus> {
        let mut out = Vec::new();
        for s in self.sessions.iter() {
            let session = s.value();
            out.push(ServerStatus {
                language: session.language,
                workspace_root: session.workspace_root.clone(),
                binary_path: Some(session.binary_path.clone()),
                state: session.state().await,
                capabilities: session.capabilities().await,
                last_error: session.last_error().await,
                restart_count: session.restart_count(),
            });
        }
        out
    }

    /// Pre-warm a language by spawning its session in the
    /// background. Errors logged via `tracing::warn!` and
    /// otherwise swallowed — this is best-effort.
    pub fn prewarm(self: &Arc<Self>, language: LspLanguage, workspace_root: PathBuf) {
        let arc = Arc::clone(self);
        tokio::spawn(async move {
            match arc.get_or_create_session(&workspace_root, language).await {
                Ok(s) => {
                    if let Err(e) = s.ensure_started().await {
                        tracing::warn!(
                            target: "lsp::manager::prewarm",
                            language = %language,
                            error = %e,
                            "[lsp] prewarm failed"
                        );
                    } else {
                        tracing::info!(
                            target: "lsp::manager::prewarm",
                            language = %language,
                            "[lsp] prewarm ok"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        target: "lsp::manager::prewarm",
                        language = %language,
                        error = %e,
                        "[lsp] prewarm: get_or_create_session failed"
                    );
                }
            }
        });
    }

    /// Resolve the session for `(workspace, language)`, creating it
    /// on first call. Returns `Unavailable` placeholder when the
    /// binary is missing on PATH so the caller still gets a session
    /// to mark `Unavailable`.
    pub async fn get_or_create_session(
        &self,
        workspace_root: &Path,
        language: LspLanguage,
    ) -> Result<Arc<LspSession>, LspError> {
        let key = (workspace_root.to_path_buf(), language);
        if let Some(existing) = self.sessions.get(&key) {
            return Ok(Arc::clone(existing.value()));
        }
        // Build a fresh session. If the launcher does not have the
        // binary, emit an Unavailable placeholder so the caller can
        // still observe a session and `mark_unavailable` it.
        let session = match session_from_launcher(
            &self.launcher,
            language,
            workspace_root.to_path_buf(),
            self.config.clone(),
        ) {
            Some(s) => Arc::new(s),
            None => {
                let s = Arc::new(LspSession::new(
                    language,
                    workspace_root.to_path_buf(),
                    PathBuf::new(),
                    self.config.clone(),
                ));
                s.mark_unavailable().await;
                s
            }
        };
        self.sessions.entry(key).or_insert(session.clone());
        Ok(session)
    }

    /// Per-binding entry point. Validates the request against the
    /// policy whitelist, routes by extension to the right session,
    /// runs the LSP request, formats output.
    ///
    /// `is_origin_synthetic` tracks whether the call came from a
    /// Phase 19/20 poller — synthetic calls do NOT update
    /// `last_activity`, so the idle reaper can still tear down the
    /// session under poller-only load.
    pub async fn execute(
        self: &Arc<Self>,
        req: LspRequest,
        policy: &ExecutePolicy,
        workspace_root: &Path,
        is_origin_synthetic: bool,
    ) -> Result<LspToolOutput, LspError> {
        let language = match &req {
            LspRequest::WorkspaceSymbol { .. } => {
                // Pick any allowed + discovered language; default to
                // first matrix entry the launcher discovered.
                let mut candidate = None;
                for lang in self.launcher.discovered_languages() {
                    if policy.permits(lang) {
                        candidate = Some(lang);
                        break;
                    }
                }
                candidate.ok_or(LspError::ServerUnavailable {
                    language: LspLanguage::Rust,
                    binary: "rust-analyzer",
                    install_hint: "rustup component add rust-analyzer",
                })?
            }
            other => {
                let file = other.file().expect("non-WorkspaceSymbol carries a file");
                let ext = std::path::Path::new(file)
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| format!(".{s}"))
                    .unwrap_or_default();
                language_for_extension(&ext).ok_or(LspError::NoServerForExtension {
                    ext: ext.clone(),
                    path: file.to_string(),
                })?
            }
        };

        if !policy.permits(language) {
            return Err(LspError::LanguageNotEnabled { language });
        }

        let session = self.get_or_create_session(workspace_root, language).await?;
        session.touch(is_origin_synthetic);

        // Dispatch.
        match req {
            LspRequest::GoToDef {
                file,
                line,
                character,
            } => {
                self.run_position_request(
                    &session,
                    workspace_root,
                    &file,
                    line,
                    character,
                    "textDocument/definition",
                )
                .await
            }
            LspRequest::Hover {
                file,
                line,
                character,
            } => {
                let abs = resolve_input_path(&file, workspace_root);
                let uri = session.ensure_open(&abs).await?;
                let pos = serde_json::json!({
                    "textDocument": { "uri": uri.as_str() },
                    "position": {
                        "line": line.saturating_sub(1),
                        "character": character.saturating_sub(1),
                    }
                });
                let raw = session
                    .request_with_retry("textDocument/hover", pos)
                    .await?;
                let hover: Option<lsp_types::Hover> =
                    serde_json::from_value(raw.clone()).unwrap_or(None);
                let formatted = match hover {
                    Some(h) => format_hover(&h),
                    None => "(no hover)".to_string(),
                };
                Ok(build_output(formatted, raw))
            }
            LspRequest::References {
                file,
                line,
                character,
            } => {
                let abs = resolve_input_path(&file, workspace_root);
                let uri = session.ensure_open(&abs).await?;
                let pos = serde_json::json!({
                    "textDocument": { "uri": uri.as_str() },
                    "position": {
                        "line": line.saturating_sub(1),
                        "character": character.saturating_sub(1),
                    },
                    "context": { "includeDeclaration": true }
                });
                let raw = session
                    .request_with_retry("textDocument/references", pos)
                    .await?;
                let locations: Vec<lsp_types::Location> =
                    serde_json::from_value(raw.clone()).unwrap_or_default();
                let formatted = format_locations(&locations, Some(workspace_root));
                Ok(build_output(formatted, raw))
            }
            LspRequest::WorkspaceSymbol { query } => {
                // We didn't open a file; the session must already be
                // running so workspace_symbol has indexed targets.
                session.ensure_started().await?;
                let raw = session
                    .request_with_retry("workspace/symbol", serde_json::json!({ "query": query }))
                    .await?;
                let symbols: Vec<lsp_types::SymbolInformation> =
                    serde_json::from_value(raw.clone()).unwrap_or_default();
                let formatted = format_workspace_symbols(&symbols, Some(workspace_root));
                Ok(build_output(formatted, raw))
            }
            LspRequest::Diagnostics { file } => {
                let abs = resolve_input_path(&file, workspace_root);
                let uri = session.ensure_open(&abs).await?;
                let snapshot: Option<FileDiagnostics> = session.diagnostics_for(&uri).await?;
                let items = snapshot
                    .as_ref()
                    .map(|s| s.items.clone())
                    .unwrap_or_default();
                let formatted = format_diagnostics(&abs, Some(workspace_root), &items);
                let structured = serde_json::to_value(&items).unwrap_or(serde_json::Value::Null);
                Ok(build_output(formatted, structured))
            }
        }
    }

    async fn run_position_request(
        &self,
        session: &Arc<LspSession>,
        workspace_root: &Path,
        file: &str,
        line: u32,
        character: u32,
        method: &str,
    ) -> Result<LspToolOutput, LspError> {
        let abs = resolve_input_path(file, workspace_root);
        let uri = session.ensure_open(&abs).await?;
        let pos = serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "position": {
                "line": line.saturating_sub(1),
                "character": character.saturating_sub(1),
            }
        });
        let raw = session.request_with_retry(method, pos).await?;
        // textDocument/definition can return Location, Location[],
        // LocationLink[], or null. Try each.
        let formatted = if raw.is_null() {
            "(no definition)".to_string()
        } else if let Ok(single) = serde_json::from_value::<lsp_types::Location>(raw.clone()) {
            format_location(&single, Some(workspace_root))
        } else if let Ok(many) = serde_json::from_value::<Vec<lsp_types::Location>>(raw.clone()) {
            format_locations(&many, Some(workspace_root))
        } else if let Ok(links) =
            serde_json::from_value::<Vec<lsp_types::LocationLink>>(raw.clone())
        {
            let locs: Vec<lsp_types::Location> = links
                .into_iter()
                .map(|l| lsp_types::Location {
                    uri: l.target_uri,
                    range: l.target_selection_range,
                })
                .collect();
            format_locations(&locs, Some(workspace_root))
        } else {
            "(unrecognised LSP result shape)".to_string()
        };
        Ok(build_output(formatted, raw))
    }

    /// Cancel the idle reaper + shut down every session. Best-
    /// effort. Called from `src/main.rs` SIGTERM handler.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let mut handles = Vec::new();
        for s in self.sessions.iter() {
            let session = Arc::clone(s.value());
            handles.push(tokio::spawn(async move { session.shutdown().await }));
        }
        for h in handles {
            let _ = h.await;
        }
        self.sessions.clear();
        if let Some(handle) = self.idle_reaper.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
        }
    }
}

async fn idle_reaper_loop(
    manager: std::sync::Weak<LspManager>,
    idle_teardown: Duration,
    cancel: CancellationToken,
) {
    let tick = idle_teardown.min(Duration::from_secs(60));
    let tick = tick.max(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(
                    target: "lsp::manager::idle_reaper",
                    "[lsp] idle reaper cancelled"
                );
                return;
            }
            _ = tokio::time::sleep(tick) => {}
        }
        let manager = match manager.upgrade() {
            Some(m) => m,
            None => return, // Manager dropped — exit.
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let teardown_threshold_ms = idle_teardown.as_millis() as i64;
        // Two-pass: collect victim keys without holding mutating
        // iterators on DashMap.
        let mut victims: Vec<(PathBuf, LspLanguage)> = Vec::new();
        for s in manager.sessions.iter() {
            let session = s.value();
            let state = session.state().await;
            if !matches!(state, SessionState::Running) {
                continue;
            }
            let idle = now - session.last_activity_ms();
            if idle >= teardown_threshold_ms {
                victims.push(s.key().clone());
            }
        }
        for key in victims {
            if let Some((_, session)) = manager.sessions.remove(&key) {
                tracing::info!(
                    target: "lsp::manager::idle_reaper",
                    language = %session.language,
                    workspace = %session.workspace_root.display(),
                    "[lsp] idle teardown"
                );
                session.shutdown().await;
                // Snapshot timing for noisy debug.
                let _ = Instant::now();
            }
        }
    }
}

/// Boot helper called once from `src/main.rs`. Probes binaries,
/// builds the manager, optionally pre-warms the supplied
/// languages.
pub async fn boot(
    enabled_languages: &[LspLanguage],
    prewarm_languages: &[LspLanguage],
    workspace_root: &Path,
) -> Arc<LspManager> {
    let _ = enabled_languages; // currently advisory; whitelist enforcement is per-binding
    let manager = LspManager::new(SessionConfig::default());
    for lang in prewarm_languages {
        manager.prewarm(*lang, workspace_root.to_path_buf());
    }
    manager
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_launcher() -> LspLauncher {
        LspLauncher::probe_with(|_| None)
    }

    #[tokio::test]
    async fn execute_policy_permits_when_empty() {
        let p = ExecutePolicy::default();
        for lang in LspLanguage::all() {
            assert!(p.permits(*lang));
        }
    }

    #[tokio::test]
    async fn execute_policy_permits_only_whitelist() {
        let p = ExecutePolicy {
            allowed_languages: vec![LspLanguage::Rust],
        };
        assert!(p.permits(LspLanguage::Rust));
        assert!(!p.permits(LspLanguage::Python));
    }

    #[tokio::test]
    async fn execute_routes_by_extension() {
        let manager = LspManager::with_launcher(empty_launcher(), SessionConfig::default());
        let req = LspRequest::Hover {
            file: "src/foo.unknown_ext".into(),
            line: 1,
            character: 1,
        };
        let err = manager
            .execute(
                req,
                &ExecutePolicy::default(),
                std::path::Path::new("/tmp"),
                false,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LspError::NoServerForExtension { .. }));
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn execute_returns_unavailable_when_binary_missing() {
        // Create a real file so the file-exists check passes; the
        // Unavailable error must come from the session FSM, not from
        // missing-on-disk.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("foo.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let manager = LspManager::with_launcher(empty_launcher(), SessionConfig::default());
        let req = LspRequest::Hover {
            file: file.to_string_lossy().into_owned(),
            line: 1,
            character: 1,
        };
        let err = manager
            .execute(req, &ExecutePolicy::default(), dir.path(), false)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                LspError::ServerUnavailable {
                    language: LspLanguage::Rust,
                    ..
                }
            ),
            "got: {err:?}"
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn execute_returns_language_not_enabled_when_whitelist_excludes() {
        let launcher = LspLauncher::probe_with(|binary| {
            // pretend rust-analyzer + pylsp are both present.
            match binary {
                "rust-analyzer" => Some(PathBuf::from("/fake/rust-analyzer")),
                "pylsp" => Some(PathBuf::from("/fake/pylsp")),
                _ => None,
            }
        });
        let manager = LspManager::with_launcher(launcher, SessionConfig::default());
        let req = LspRequest::Hover {
            file: "src/foo.py".into(),
            line: 1,
            character: 1,
        };
        let policy = ExecutePolicy {
            allowed_languages: vec![LspLanguage::Rust],
        };
        let err = manager
            .execute(req, &policy, std::path::Path::new("/tmp"), false)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                LspError::LanguageNotEnabled {
                    language: LspLanguage::Python
                }
            ),
            "got: {err:?}"
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn aggregated_capabilities_or_across_sessions() {
        let manager = LspManager::with_launcher(empty_launcher(), SessionConfig::default());
        // No sessions yet → all flags false.
        let caps = manager.aggregated_capabilities().await;
        assert!(!caps.definition);
        assert!(!caps.hover);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_cancels_idle_reaper() {
        let manager = LspManager::with_launcher(empty_launcher(), SessionConfig::default());
        // Just exercise the shutdown path — must not deadlock.
        manager.shutdown().await;
    }
}
