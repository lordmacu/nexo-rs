//! Pure-data types: language enum, language matrix, request
//! discriminator, capabilities snapshot, session state, status,
//! tool output.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/LSPTool/schemas.ts:13-191` —
//!     discriminated union per `operation`. We collapse the 9 ops
//!     into 5 for the MVP (go_to_def, hover, references,
//!     workspace_symbol, diagnostics).
//!   * `claude-code-leak/src/services/lsp/LSPServerInstance.ts:74-150`
//!     — FSM states. We add an explicit `Unavailable` for binaries
//!     missing on PATH so the manager never tries to spawn them.
//!
//! Spec citation: `proyecto/PHASES.md:4955-4963` defines the
//! built-in matrix.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One of the four built-in languages backed by [`LANGUAGE_MATRIX`].
/// Adding a fifth means extending both the enum and the matrix and
/// re-running unit tests in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LspLanguage {
    Rust,
    Python,
    TypeScript,
    Go,
}

impl LspLanguage {
    /// Iterates the variants in matrix order. Used by the launcher
    /// to walk the probe set deterministically.
    pub fn all() -> &'static [LspLanguage] {
        &[
            LspLanguage::Rust,
            LspLanguage::Python,
            LspLanguage::TypeScript,
            LspLanguage::Go,
        ]
    }
}

impl std::fmt::Display for LspLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LspLanguage::Rust => "rust",
            LspLanguage::Python => "python",
            LspLanguage::TypeScript => "typescript",
            LspLanguage::Go => "go",
        };
        f.write_str(s)
    }
}

/// Static matrix entry: binary name + install hint surfaced when
/// the binary is missing + file extensions routed to this language.
#[derive(Debug, Clone, Copy)]
pub struct LanguageMatrixEntry {
    pub language: LspLanguage,
    pub binary: &'static str,
    pub install_hint: &'static str,
    /// Lowercase, *with* leading dot. Single source of truth for
    /// extension → language routing.
    pub extensions: &'static [&'static str],
}

/// Built-in matrix. Order = probe order = whitelist iteration order.
/// Lifted from `proyecto/PHASES.md:4955-4963`.
pub const LANGUAGE_MATRIX: &[LanguageMatrixEntry] = &[
    LanguageMatrixEntry {
        language: LspLanguage::Rust,
        binary: "rust-analyzer",
        install_hint: "rustup component add rust-analyzer",
        extensions: &[".rs"],
    },
    LanguageMatrixEntry {
        language: LspLanguage::Python,
        binary: "pylsp",
        install_hint: "pip install python-lsp-server",
        extensions: &[".py", ".pyi"],
    },
    LanguageMatrixEntry {
        language: LspLanguage::TypeScript,
        binary: "typescript-language-server",
        install_hint: "npm i -g typescript-language-server",
        extensions: &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"],
    },
    LanguageMatrixEntry {
        language: LspLanguage::Go,
        binary: "gopls",
        install_hint: "go install golang.org/x/tools/gopls@latest",
        extensions: &[".go"],
    },
];

/// Lookup the language responsible for an extension. Case-insensitive
/// on the input. `Some(lang)` only when the extension is a member of
/// the matrix; unknown extensions return `None` (the call site surfaces
/// `LspError::NoServerForExtension`).
pub fn language_for_extension(ext_with_dot: &str) -> Option<LspLanguage> {
    let lower = ext_with_dot.to_ascii_lowercase();
    for entry in LANGUAGE_MATRIX {
        if entry.extensions.iter().any(|e| *e == lower.as_str()) {
            return Some(entry.language);
        }
    }
    None
}

/// Lookup the matrix entry for a language. Stable; does not allocate.
pub fn matrix_entry(lang: LspLanguage) -> &'static LanguageMatrixEntry {
    LANGUAGE_MATRIX
        .iter()
        .find(|e| e.language == lang)
        .expect("LANGUAGE_MATRIX missing entry for declared LspLanguage variant")
}

/// 5-op MVP request shape. Discriminated by `kind` for serde wire
/// compatibility with the LLM tool surface. Lift from
/// `claude-code-leak/src/tools/LSPTool/schemas.ts:13-191` collapsed
/// to MVP set.
///
/// `line` and `character` are **1-based** to match editor UX —
/// the same convention the leak uses
/// (`claude-code-leak/src/tools/LSPTool/schemas.ts:75-84`). The
/// session converts to 0-based on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LspRequest {
    GoToDef {
        file: String,
        line: u32,
        character: u32,
    },
    Hover {
        file: String,
        line: u32,
        character: u32,
    },
    References {
        file: String,
        line: u32,
        character: u32,
    },
    WorkspaceSymbol {
        query: String,
    },
    Diagnostics {
        file: String,
    },
}

impl LspRequest {
    /// File path the request operates on. `WorkspaceSymbol` has no
    /// per-file scope; the manager routes it to one server arbitrarily
    /// (the first running session).
    pub fn file(&self) -> Option<&str> {
        match self {
            LspRequest::GoToDef { file, .. }
            | LspRequest::Hover { file, .. }
            | LspRequest::References { file, .. }
            | LspRequest::Diagnostics { file } => Some(file.as_str()),
            LspRequest::WorkspaceSymbol { .. } => None,
        }
    }

    /// Tool-result-friendly kind name (matches the discriminator on
    /// the wire). Used by formatters and audit log.
    pub fn kind(&self) -> &'static str {
        match self {
            LspRequest::GoToDef { .. } => "go_to_def",
            LspRequest::Hover { .. } => "hover",
            LspRequest::References { .. } => "references",
            LspRequest::WorkspaceSymbol { .. } => "workspace_symbol",
            LspRequest::Diagnostics { .. } => "diagnostics",
        }
    }
}

/// Subset of `lsp_types::ServerCapabilities` we care about, normalised
/// to bool flags. Sourced from the `initialize` response in
/// `client.rs`. `OR`-aggregated across running sessions in
/// `manager.rs::aggregated_capabilities()` to drive the dynamic
/// `Lsp` tool description.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCapabilities {
    pub definition: bool,
    pub references: bool,
    pub hover: bool,
    pub workspace_symbol: bool,
    /// Server pushes `textDocument/publishDiagnostics`. Always true
    /// in practice; we still track it for parity with the leak's
    /// `passiveFeedback.ts` registration gate.
    pub publish_diagnostics: bool,
}

impl ResolvedCapabilities {
    /// Bitwise OR — used by the manager to fold per-session caps
    /// into a single advertised set.
    pub fn or(self, other: Self) -> Self {
        Self {
            definition: self.definition | other.definition,
            references: self.references | other.references,
            hover: self.hover | other.hover,
            workspace_symbol: self.workspace_symbol | other.workspace_symbol,
            publish_diagnostics: self.publish_diagnostics | other.publish_diagnostics,
        }
    }

    /// Whether the given `kind` would be served by any session
    /// reporting this capability set. Used to filter the dynamic
    /// `Lsp` tool description.
    pub fn supports(&self, kind: &str) -> bool {
        match kind {
            "go_to_def" => self.definition,
            "references" => self.references,
            "hover" => self.hover,
            "workspace_symbol" => self.workspace_symbol,
            "diagnostics" => self.publish_diagnostics,
            _ => false,
        }
    }
}

/// FSM state per session. Mirrors the leak's
/// `LSPServerInstance.ts:74-150` plus an explicit `Unavailable`
/// for binaries missing on PATH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Stopped,
    Starting,
    Running,
    Stopping,
    /// Process exited with non-zero or LSP handshake failed. Stays
    /// here until either a successful restart (cap `max_restarts =
    /// 3`) or `SessionDead`.
    Error,
    /// Binary missing on PATH at probe time. The manager never tries
    /// to spawn an unavailable session; tool calls return
    /// `LspError::ServerUnavailable` with the install hint.
    Unavailable,
}

/// Tool result shape returned to the agent. Two channels:
/// `formatted` for the LLM (terse, `path:line:col (label)`);
/// `structured` for tests + audit log (raw JSON).
#[derive(Debug, Clone)]
pub struct LspToolOutput {
    pub formatted: String,
    pub structured: serde_json::Value,
}

/// Operator-facing summary of one `(workspace, language)` session.
/// Surfaced by `LspManager::server_status()` for `nexo setup doctor`
/// and for diagnostics rendering in admin-ui (Phase 79.14 follow-up).
#[derive(Debug, Clone)]
pub struct ServerStatus {
    pub language: LspLanguage,
    pub workspace_root: PathBuf,
    pub binary_path: Option<PathBuf>,
    pub state: SessionState,
    pub capabilities: ResolvedCapabilities,
    pub last_error: Option<String>,
    pub restart_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_matrix_has_4_entries_no_extension_overlap() {
        assert_eq!(LANGUAGE_MATRIX.len(), 4);
        let mut seen = std::collections::HashSet::new();
        for entry in LANGUAGE_MATRIX {
            for ext in entry.extensions {
                assert!(
                    seen.insert(*ext),
                    "extension `{ext}` appears in two language entries"
                );
            }
        }
    }

    #[test]
    fn language_for_extension_routes_correctly() {
        assert_eq!(language_for_extension(".rs"), Some(LspLanguage::Rust));
        assert_eq!(language_for_extension(".PY"), Some(LspLanguage::Python));
        assert_eq!(language_for_extension(".tsx"), Some(LspLanguage::TypeScript));
        assert_eq!(language_for_extension(".go"), Some(LspLanguage::Go));
        assert_eq!(language_for_extension(".unknown"), None);
        assert_eq!(language_for_extension(""), None);
    }

    #[test]
    fn lsp_request_serde_roundtrip_for_each_kind() {
        for req in [
            LspRequest::GoToDef {
                file: "x.rs".into(),
                line: 1,
                character: 1,
            },
            LspRequest::Hover {
                file: "x.rs".into(),
                line: 5,
                character: 10,
            },
            LspRequest::References {
                file: "x.rs".into(),
                line: 5,
                character: 10,
            },
            LspRequest::WorkspaceSymbol {
                query: "Foo".into(),
            },
            LspRequest::Diagnostics {
                file: "x.rs".into(),
            },
        ] {
            let json = serde_json::to_string(&req).unwrap();
            let back: LspRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(req, back);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed["kind"], req.kind());
        }
    }

    #[test]
    fn matrix_entry_lookup_is_total() {
        for lang in LspLanguage::all() {
            let entry = matrix_entry(*lang);
            assert_eq!(entry.language, *lang);
            assert!(!entry.binary.is_empty());
            assert!(!entry.install_hint.is_empty());
        }
    }

    #[test]
    fn resolved_capabilities_or_is_associative() {
        let a = ResolvedCapabilities {
            definition: true,
            ..Default::default()
        };
        let b = ResolvedCapabilities {
            hover: true,
            ..Default::default()
        };
        let c = a.or(b);
        assert!(c.definition);
        assert!(c.hover);
        assert!(!c.references);
    }

    #[test]
    fn resolved_capabilities_supports_unknown_kind_is_false() {
        let caps = ResolvedCapabilities {
            definition: true,
            references: true,
            hover: true,
            workspace_symbol: true,
            publish_diagnostics: true,
        };
        assert!(caps.supports("go_to_def"));
        assert!(caps.supports("references"));
        assert!(caps.supports("hover"));
        assert!(caps.supports("workspace_symbol"));
        assert!(caps.supports("diagnostics"));
        assert!(!caps.supports("unknown_kind"));
        assert!(!caps.supports(""));
    }
}
