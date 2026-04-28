//! Per-`(workspace, language)` session: FSM around an
//! [`LspClient`], file-open cache, retry on `-32801 ContentModified`,
//! synthetic-origin-aware `last_activity` tracking.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/services/lsp/LSPServerInstance.ts:74-150`
//!     — FSM `stopped → starting → running → stopping → stopped` +
//!     `error` + `maxRestarts ?? 3`. Mirror.
//!   * `claude-code-leak/src/services/lsp/LSPServerInstance.ts:17-28`
//!     — `LSP_ERROR_CONTENT_MODIFIED = -32801`,
//!     `MAX_RETRIES_FOR_TRANSIENT_ERRORS = 3`,
//!     `RETRY_BASE_DELAY_MS = 500`. Mirror.
//!   * `claude-code-leak/src/tools/LSPTool/LSPTool.ts:53` — file
//!     size cap 10 MB. Mirror.
//!   * `claude-code-leak/src/tools/LSPTool/LSPTool.ts:259-278` —
//!     `didOpen`-on-first-touch pattern, with cached open set.
//!     Mirror.

use crate::client::{FileDiagnostics, LspClient};
use crate::error::LspError;
use crate::launcher::LspLauncher;
use crate::types::{matrix_entry, LspLanguage, ResolvedCapabilities, SessionState};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OnceCell, RwLock};

/// Per-session knobs. Defaults track the spec; tests override.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub max_restarts: u32,
    pub idle_teardown: Duration,
    pub request_timeout: Duration,
    pub max_file_bytes: u64,
    pub diagnostics_wait: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_restarts: 3,
            idle_teardown: Duration::from_secs(600),
            request_timeout: Duration::from_secs(30),
            max_file_bytes: 10_000_000, // mirror leak `MAX_LSP_FILE_SIZE_BYTES`
            diagnostics_wait: Duration::from_secs(2),
        }
    }
}

/// Numeric LSP error code for "content modified" (server reindexing).
/// Lifted from `LSPServerInstance.ts:17`.
pub const LSP_ERROR_CONTENT_MODIFIED: i64 = -32801;

/// Retry delays in ms for `ContentModified`. Mirror leak
/// `LSPServerInstance.ts:23-28`.
pub const RETRY_DELAYS_MS: [u64; 3] = [500, 1000, 2000];

/// Per-`(workspace, language)` session.
///
/// State transitions are guarded by a single `RwLock` over
/// `SessionState`; the actual LSP client lives behind `OnceCell`
/// so concurrent `ensure_started` calls do not double-spawn.
pub struct LspSession {
    pub language: LspLanguage,
    pub workspace_root: PathBuf,
    pub binary_path: PathBuf,
    state: RwLock<SessionState>,
    client: OnceCell<Arc<LspClient>>,
    capabilities: RwLock<ResolvedCapabilities>,
    open_files: Mutex<HashSet<url::Url>>,
    last_activity_ms: AtomicI64,
    restart_count: AtomicU32,
    last_error: Mutex<Option<String>>,
    config: SessionConfig,
}

impl LspSession {
    /// Construct a new session pointing at the given binary +
    /// workspace. The session is `Stopped` until `ensure_started`
    /// is called; `Unavailable` is set externally by the manager
    /// when the launcher reports the binary missing.
    pub fn new(
        language: LspLanguage,
        workspace_root: PathBuf,
        binary_path: PathBuf,
        config: SessionConfig,
    ) -> Self {
        Self {
            language,
            workspace_root,
            binary_path,
            state: RwLock::new(SessionState::Stopped),
            client: OnceCell::new(),
            capabilities: RwLock::new(ResolvedCapabilities::default()),
            open_files: Mutex::new(HashSet::new()),
            last_activity_ms: AtomicI64::new(now_ms()),
            restart_count: AtomicU32::new(0),
            last_error: Mutex::new(None),
            config,
        }
    }

    pub async fn state(&self) -> SessionState {
        *self.state.read().await
    }

    pub async fn capabilities(&self) -> ResolvedCapabilities {
        *self.capabilities.read().await
    }

    pub async fn last_error(&self) -> Option<String> {
        self.last_error.lock().await.clone()
    }

    pub fn restart_count(&self) -> u32 {
        self.restart_count.load(Ordering::Relaxed)
    }

    pub fn last_activity_ms(&self) -> i64 {
        self.last_activity_ms.load(Ordering::Relaxed)
    }

    /// Update `last_activity_ms` UNLESS the call is
    /// synthetic-origin (Phase 19/20 poller). Synthetic callers
    /// must not keep the session warm or we never idle-teardown.
    pub fn touch(&self, is_origin_synthetic: bool) {
        if !is_origin_synthetic {
            self.last_activity_ms.store(now_ms(), Ordering::Relaxed);
        }
    }

    /// Lazy-spawn the underlying [`LspClient`] when state allows.
    /// On crash recovery (`state == Error`) the restart counter
    /// must be below the cap or this returns
    /// [`LspError::SessionDead`].
    pub async fn ensure_started(&self) -> Result<Arc<LspClient>, LspError> {
        // Fast path — already running.
        if let Some(c) = self.client.get() {
            if matches!(*self.state.read().await, SessionState::Running) {
                return Ok(Arc::clone(c));
            }
        }

        // Acquire write lock and re-check.
        let mut state = self.state.write().await;
        match *state {
            SessionState::Running => {
                // Another caller raced us — done.
                if let Some(c) = self.client.get() {
                    return Ok(Arc::clone(c));
                }
            }
            SessionState::Starting | SessionState::Stopping => {
                drop(state);
                // Yield + retry. Bounded — we re-check up to 50 ms.
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Box::pin(self.ensure_started()).await;
            }
            SessionState::Unavailable => {
                let entry = matrix_entry(self.language);
                return Err(LspError::ServerUnavailable {
                    language: self.language,
                    binary: entry.binary,
                    install_hint: entry.install_hint,
                });
            }
            SessionState::Error => {
                let count = self.restart_count.load(Ordering::Relaxed);
                if count >= self.config.max_restarts {
                    return Err(LspError::SessionDead {
                        language: self.language,
                    });
                }
                self.restart_count.fetch_add(1, Ordering::Relaxed);
            }
            SessionState::Stopped => {}
        }

        *state = SessionState::Starting;
        drop(state);

        match self.do_start().await {
            Ok(client) => {
                *self.state.write().await = SessionState::Running;
                let arc = self
                    .client
                    .get_or_init(|| async { client })
                    .await
                    .clone();
                tracing::info!(
                    target: "lsp::session::start",
                    language = %self.language,
                    workspace = %self.workspace_root.display(),
                    "[lsp] session running"
                );
                Ok(arc)
            }
            Err(e) => {
                *self.state.write().await = SessionState::Error;
                *self.last_error.lock().await = Some(e.to_string());
                tracing::warn!(
                    target: "lsp::session::start",
                    language = %self.language,
                    error = %e,
                    "[lsp] session start failed"
                );
                Err(e)
            }
        }
    }

    async fn do_start(&self) -> Result<Arc<LspClient>, LspError> {
        let env = std::collections::HashMap::new();
        let client = LspClient::spawn(
            &self.binary_path,
            &self.workspace_root,
            &env,
            self.config.request_timeout,
        )
        .await?;
        let caps = client.initialize(&self.workspace_root).await?;
        *self.capabilities.write().await = caps;
        Ok(Arc::new(client))
    }

    /// Open `path` in the LSP server if it isn't already. No-op
    /// when already in the cache. Reads the file content from
    /// disk; checks the 10 MB cap before reading.
    pub async fn ensure_open(&self, path: &Path) -> Result<url::Url, LspError> {
        let absolute = path.canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LspError::FileNotFound {
                path: path.display().to_string(),
            },
            _ => LspError::Wire(format!("canonicalize `{}` failed: {e}", path.display())),
        })?;
        let metadata = std::fs::metadata(&absolute).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LspError::FileNotFound {
                path: absolute.display().to_string(),
            },
            _ => LspError::Wire(format!("stat `{}` failed: {e}", absolute.display())),
        })?;
        if !metadata.is_file() {
            return Err(LspError::NotAFile {
                path: absolute.display().to_string(),
            });
        }
        if metadata.len() > self.config.max_file_bytes {
            return Err(LspError::FileTooLarge {
                path: absolute.display().to_string(),
                max_bytes: self.config.max_file_bytes,
                actual_bytes: metadata.len(),
            });
        }

        let uri = url::Url::from_file_path(&absolute).map_err(|_| {
            LspError::Wire(format!(
                "could not convert `{}` to file URI",
                absolute.display()
            ))
        })?;

        let client = self.ensure_started().await?;
        let already_open = self.open_files.lock().await.contains(&uri);
        if !already_open {
            let content = std::fs::read_to_string(&absolute).map_err(|e| {
                LspError::Wire(format!("read `{}` failed: {e}", absolute.display()))
            })?;
            let language_id = match self.language {
                LspLanguage::Rust => "rust",
                LspLanguage::Python => "python",
                LspLanguage::TypeScript => match absolute
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                {
                    "tsx" => "typescriptreact",
                    "jsx" => "javascriptreact",
                    "js" | "mjs" | "cjs" => "javascript",
                    _ => "typescript",
                },
                LspLanguage::Go => "go",
            };
            client.notify_raw(
                "textDocument/didOpen",
                serde_json::json!({
                    "textDocument": {
                        "uri": uri.as_str(),
                        "languageId": language_id,
                        "version": 1,
                        "text": content,
                    }
                }),
            )?;
            self.open_files.lock().await.insert(uri.clone());
        }
        Ok(uri)
    }

    /// Issue an LSP request with the 500 / 1000 / 2000 ms backoff
    /// on `-32801 ContentModified`. Other wire errors propagate
    /// unchanged.
    pub async fn request_with_retry(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LspError> {
        let client = self.ensure_started().await?;
        let mut attempt = 0;
        loop {
            match client.request_raw(method, params.clone()).await {
                Ok(v) => return Ok(v),
                Err(LspError::Wire(msg))
                    if attempt < RETRY_DELAYS_MS.len()
                        && msg.contains(&format!("\"code\":{LSP_ERROR_CONTENT_MODIFIED}")) =>
                {
                    let delay = Duration::from_millis(RETRY_DELAYS_MS[attempt]);
                    tracing::warn!(
                        target: "lsp::session::request_retry",
                        language = %self.language,
                        method,
                        attempt = attempt + 1,
                        code = LSP_ERROR_CONTENT_MODIFIED,
                        delay_ms = delay.as_millis() as u64,
                        "[lsp] retrying after ContentModified"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Returns the cached diagnostics for `uri`, optionally waiting
    /// up to `diagnostics_wait` if no snapshot is present yet
    /// (e.g. file was just opened).
    pub async fn diagnostics_for(
        &self,
        uri: &url::Url,
    ) -> Result<Option<FileDiagnostics>, LspError> {
        let client = self.ensure_started().await?;
        if let Some(d) = client.diagnostics.get(uri).await {
            return Ok(Some(d));
        }
        let deadline = tokio::time::Instant::now() + self.config.diagnostics_wait;
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(d) = client.diagnostics.get(uri).await {
                return Ok(Some(d));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    /// Stop the underlying client (best-effort) and reset state to
    /// `Stopped`. Idle reaper + manager shutdown both call this.
    pub async fn shutdown(&self) {
        {
            let mut state = self.state.write().await;
            if matches!(*state, SessionState::Stopped | SessionState::Unavailable) {
                return;
            }
            *state = SessionState::Stopping;
        }
        if let Some(client) = self.client.get() {
            client.shutdown(Duration::from_secs(3)).await;
        }
        self.open_files.lock().await.clear();
        *self.state.write().await = SessionState::Stopped;
        tracing::info!(
            target: "lsp::session::shutdown",
            language = %self.language,
            "[lsp] session stopped"
        );
    }

    /// Mark the session as `Unavailable` — used by the manager
    /// when the launcher reports the binary missing.
    pub async fn mark_unavailable(&self) {
        *self.state.write().await = SessionState::Unavailable;
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Convenience: build a session, taking the launcher's discovered
/// binary path. Returns `None` when the language is unavailable
/// (caller substitutes an `Unavailable` placeholder if needed).
pub fn session_from_launcher(
    launcher: &LspLauncher,
    language: LspLanguage,
    workspace_root: PathBuf,
    config: SessionConfig,
) -> Option<LspSession> {
    let binary = launcher.binary_for(language)?;
    Some(LspSession::new(
        language,
        workspace_root,
        binary.to_path_buf(),
        config,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn session_new_starts_in_stopped_state() {
        let s = LspSession::new(
            LspLanguage::Rust,
            PathBuf::from("/tmp"),
            PathBuf::from("/usr/bin/rust-analyzer"),
            SessionConfig::default(),
        );
        assert_eq!(s.state().await, SessionState::Stopped);
        assert_eq!(s.restart_count(), 0);
        assert_eq!(s.capabilities().await, ResolvedCapabilities::default());
    }

    #[tokio::test]
    async fn touch_synthetic_origin_does_not_update_activity() {
        let s = LspSession::new(
            LspLanguage::Rust,
            PathBuf::from("/tmp"),
            PathBuf::from("/usr/bin/rust-analyzer"),
            SessionConfig::default(),
        );
        let before = s.last_activity_ms();
        // Wait so any update would be measurable.
        tokio::time::sleep(Duration::from_millis(2)).await;
        s.touch(true);
        assert_eq!(s.last_activity_ms(), before, "synthetic origin must not touch");
        s.touch(false);
        assert!(
            s.last_activity_ms() > before,
            "non-synthetic origin must update"
        );
    }

    #[tokio::test]
    async fn mark_unavailable_short_circuits_ensure_started() {
        let s = LspSession::new(
            LspLanguage::Rust,
            PathBuf::from("/tmp"),
            // Path doesn't exist; we never spawn because we mark
            // unavailable first.
            PathBuf::from("/nonexistent/rust-analyzer"),
            SessionConfig::default(),
        );
        s.mark_unavailable().await;
        let err = s.ensure_started().await.unwrap_err();
        assert!(matches!(err, LspError::ServerUnavailable { language: LspLanguage::Rust, .. }));
    }

    #[tokio::test]
    async fn ensure_open_file_too_large_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let big_path = dir.path().join("big.rs");
        let big_content = "x".repeat(11_000_000);
        std::fs::write(&big_path, big_content).unwrap();

        let mut cfg = SessionConfig::default();
        cfg.max_file_bytes = 10_000_000;
        let s = LspSession::new(
            LspLanguage::Rust,
            dir.path().to_path_buf(),
            PathBuf::from("/nonexistent/rust-analyzer"),
            cfg,
        );
        // Mark unavailable so ensure_open exits before spawn — the
        // file-size check should still fire because it precedes
        // ensure_started.
        s.mark_unavailable().await;
        let err = s.ensure_open(&big_path).await.unwrap_err();
        assert!(matches!(err, LspError::FileTooLarge { .. }));
    }

    #[tokio::test]
    async fn ensure_open_file_not_found_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = LspSession::new(
            LspLanguage::Rust,
            dir.path().to_path_buf(),
            PathBuf::from("/nonexistent/rust-analyzer"),
            SessionConfig::default(),
        );
        s.mark_unavailable().await;
        let err = s
            .ensure_open(&dir.path().join("missing.rs"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, LspError::FileNotFound { .. }),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn ensure_open_directory_returns_not_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = LspSession::new(
            LspLanguage::Rust,
            dir.path().to_path_buf(),
            PathBuf::from("/nonexistent/rust-analyzer"),
            SessionConfig::default(),
        );
        s.mark_unavailable().await;
        let err = s.ensure_open(dir.path()).await.unwrap_err();
        assert!(matches!(err, LspError::NotAFile { .. }), "got: {err:?}");
    }

    #[tokio::test]
    async fn restart_count_caps_at_max_restarts() {
        let mut cfg = SessionConfig::default();
        cfg.max_restarts = 2;
        let s = LspSession::new(
            LspLanguage::Rust,
            PathBuf::from("/tmp"),
            PathBuf::from("/nonexistent/rust-analyzer"),
            cfg,
        );
        // Force into Error state directly.
        *s.state.write().await = SessionState::Error;
        // First retry uses up restart slot 0 → 1, fails → Error.
        // Second retry: 1 → 2, fails → Error.
        // Third retry: 2 >= max(2) → SessionDead.
        s.restart_count.store(2, Ordering::Relaxed);
        let err = s.ensure_started().await.unwrap_err();
        assert!(matches!(err, LspError::SessionDead { .. }), "got: {err:?}");
    }

    #[tokio::test]
    async fn session_from_launcher_returns_none_when_missing() {
        let launcher = LspLauncher::probe_with(|_| None);
        let s = session_from_launcher(
            &launcher,
            LspLanguage::Rust,
            PathBuf::from("/tmp"),
            SessionConfig::default(),
        );
        assert!(s.is_none());
    }

    #[tokio::test]
    async fn session_from_launcher_returns_some_when_present() {
        let launcher = LspLauncher::probe_with(|binary| {
            if binary == "rust-analyzer" {
                Some(PathBuf::from("/fake/rust-analyzer"))
            } else {
                None
            }
        });
        let s = session_from_launcher(
            &launcher,
            LspLanguage::Rust,
            PathBuf::from("/tmp"),
            SessionConfig::default(),
        );
        assert!(s.is_some());
        assert_eq!(s.unwrap().language, LspLanguage::Rust);
    }

    #[tokio::test]
    async fn session_config_default_matches_spec() {
        let cfg = SessionConfig::default();
        assert_eq!(cfg.max_restarts, 3);
        assert_eq!(cfg.idle_teardown, Duration::from_secs(600));
        assert_eq!(cfg.request_timeout, Duration::from_secs(30));
        assert_eq!(cfg.max_file_bytes, 10_000_000);
        assert_eq!(cfg.diagnostics_wait, Duration::from_secs(2));
    }
}
