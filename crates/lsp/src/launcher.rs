//! Boot-time `which`-probe of the four built-in LSP binaries.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/services/lsp/LSPServerInstance.ts:158-161`
//!     — spawn args reading `config.command`. We probe BEFORE
//!     spawn (the leak relies on plugin contributions and only
//!     fails at first request) so the `Lsp` tool description
//!     never advertises a kind backed exclusively by a missing
//!     binary.
//!
//! Reference (secondary):
//!   * `research/src/agents/pi-bundle-lsp-runtime.ts:294-340` —
//!     OpenClaw also defers to plugin contribution, no built-in
//!     matrix.

use crate::types::{matrix_entry, LspLanguage, LANGUAGE_MATRIX};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Result of a one-shot probe over [`LANGUAGE_MATRIX`]. Caller
/// keeps the [`LspLauncher`] for the lifetime of the daemon and
/// reuses it across sessions.
pub struct LspLauncher {
    discovered: HashMap<LspLanguage, PathBuf>,
}

impl LspLauncher {
    /// Walks every entry in [`LANGUAGE_MATRIX`], calls
    /// [`which::which`] per binary, emits a single `tracing::warn!`
    /// for each missing binary (with the install hint), and stores
    /// the resolved absolute path for the present ones.
    ///
    /// Idempotent — call once at boot. Subsequent probes are
    /// allowed (operator runs `nexo setup reload` after
    /// `rustup component add rust-analyzer`) and will pick up
    /// newly-installed binaries; existing sessions are not
    /// disturbed by re-probing.
    pub fn probe() -> Self {
        Self::probe_with(|binary| which::which(binary).ok())
    }

    /// Test-only probe with an injected resolver. The resolver
    /// returns `Some(path)` when the binary should be considered
    /// present.
    pub fn probe_with<F>(mut resolve: F) -> Self
    where
        F: FnMut(&str) -> Option<PathBuf>,
    {
        let mut discovered = HashMap::new();
        for entry in LANGUAGE_MATRIX {
            match resolve(entry.binary) {
                Some(path) => {
                    tracing::info!(
                        target: "lsp::launcher",
                        language = %entry.language,
                        binary = %entry.binary,
                        path = %path.display(),
                        "[lsp] probe ok"
                    );
                    discovered.insert(entry.language, path);
                }
                None => {
                    tracing::warn!(
                        target: "lsp::launcher",
                        language = %entry.language,
                        binary = %entry.binary,
                        install_hint = %entry.install_hint,
                        "[lsp] probe missing — server will return ServerUnavailable on call"
                    );
                }
            }
        }
        Self { discovered }
    }

    /// Resolved absolute path for a language's binary, when present.
    pub fn binary_for(&self, lang: LspLanguage) -> Option<&Path> {
        self.discovered.get(&lang).map(|p| p.as_path())
    }

    /// Whether at least one server's binary was discovered. Used
    /// at boot to decide whether to spawn the idle reaper task at
    /// all (no-op when zero servers are usable).
    pub fn any_available(&self) -> bool {
        !self.discovered.is_empty()
    }

    /// Stable install hint for a language. Available for any
    /// language regardless of discovery state — the call site uses
    /// it to render `LspError::ServerUnavailable`.
    pub fn install_hint(lang: LspLanguage) -> &'static str {
        matrix_entry(lang).install_hint
    }

    /// Stable binary name for a language.
    pub fn binary_name(lang: LspLanguage) -> &'static str {
        matrix_entry(lang).binary
    }

    /// Snapshot of the discovered set — useful for diagnostics
    /// pages.
    pub fn discovered_languages(&self) -> Vec<LspLanguage> {
        let mut out: Vec<LspLanguage> = self.discovered.keys().copied().collect();
        out.sort_by_key(|l| match l {
            LspLanguage::Rust => 0,
            LspLanguage::Python => 1,
            LspLanguage::TypeScript => 2,
            LspLanguage::Go => 3,
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn probe_finds_all_when_resolver_succeeds_for_all() {
        let launcher = LspLauncher::probe_with(|binary| {
            Some(PathBuf::from(format!("/fake/bin/{binary}")))
        });
        assert!(launcher.any_available());
        assert_eq!(launcher.discovered_languages().len(), 4);
        assert_eq!(
            launcher.binary_for(LspLanguage::Rust).unwrap(),
            std::path::Path::new("/fake/bin/rust-analyzer")
        );
        assert_eq!(
            launcher.binary_for(LspLanguage::Go).unwrap(),
            std::path::Path::new("/fake/bin/gopls")
        );
    }

    #[test]
    fn probe_handles_partial_discovery() {
        // Only rust-analyzer + pylsp available.
        let launcher = LspLauncher::probe_with(|binary| match binary {
            "rust-analyzer" => Some(PathBuf::from("/usr/bin/rust-analyzer")),
            "pylsp" => Some(PathBuf::from("/home/x/.local/bin/pylsp")),
            _ => None,
        });
        assert!(launcher.any_available());
        assert!(launcher.binary_for(LspLanguage::Rust).is_some());
        assert!(launcher.binary_for(LspLanguage::Python).is_some());
        assert!(launcher.binary_for(LspLanguage::TypeScript).is_none());
        assert!(launcher.binary_for(LspLanguage::Go).is_none());
        assert_eq!(launcher.discovered_languages().len(), 2);
    }

    #[test]
    fn probe_with_zero_resolution_is_empty() {
        let launcher = LspLauncher::probe_with(|_| None);
        assert!(!launcher.any_available());
        assert!(launcher.discovered_languages().is_empty());
        for lang in LspLanguage::all() {
            assert!(launcher.binary_for(*lang).is_none());
        }
    }

    #[test]
    fn install_hint_matches_matrix() {
        assert_eq!(
            LspLauncher::install_hint(LspLanguage::Rust),
            "rustup component add rust-analyzer"
        );
        assert_eq!(
            LspLauncher::install_hint(LspLanguage::Python),
            "pip install python-lsp-server"
        );
        assert_eq!(
            LspLauncher::install_hint(LspLanguage::TypeScript),
            "npm i -g typescript-language-server"
        );
        assert_eq!(
            LspLauncher::install_hint(LspLanguage::Go),
            "go install golang.org/x/tools/gopls@latest"
        );
    }

    #[test]
    fn binary_name_matches_matrix() {
        assert_eq!(LspLauncher::binary_name(LspLanguage::Rust), "rust-analyzer");
        assert_eq!(
            LspLauncher::binary_name(LspLanguage::TypeScript),
            "typescript-language-server"
        );
    }

    #[test]
    fn discovered_languages_sorted_in_matrix_order() {
        let launcher = LspLauncher::probe_with(|binary| match binary {
            "gopls" => Some(PathBuf::from("/x/gopls")),
            "rust-analyzer" => Some(PathBuf::from("/x/rust-analyzer")),
            _ => None,
        });
        // Rust comes before Go in matrix order.
        assert_eq!(
            launcher.discovered_languages(),
            vec![LspLanguage::Rust, LspLanguage::Go]
        );
    }
}
