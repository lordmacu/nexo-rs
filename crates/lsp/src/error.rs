//! All errors the LSP client can surface to the agent runtime.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/services/lsp/LSPServerInstance.ts:17`
//!     — `LSP_ERROR_CONTENT_MODIFIED = -32801` constant lifted into
//!     `Wire` body when retries exhaust.
//!   * `claude-code-leak/src/tools/LSPTool/LSPTool.ts:53` —
//!     `MAX_LSP_FILE_SIZE_BYTES = 10_000_000` mirrored as
//!     `FileTooLarge.max_bytes`.
//!
//! Display strings are deterministic — the agent surfaces them
//! verbatim in `tool_result`, so the model can act on the install
//! hint or the timeout reason without parsing.

use crate::types::LspLanguage;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error(
        "LSP server `{language}` unavailable: `{binary}` not on PATH. Install: {install_hint}"
    )]
    ServerUnavailable {
        language: LspLanguage,
        binary: &'static str,
        install_hint: &'static str,
    },

    #[error("file `{path}` exceeds {max_bytes}-byte LSP cap (got {actual_bytes})")]
    FileTooLarge {
        path: String,
        max_bytes: u64,
        actual_bytes: u64,
    },

    #[error("file `{path}` does not exist")]
    FileNotFound { path: String },

    #[error("path `{path}` is not a regular file")]
    NotAFile { path: String },

    #[error("no LSP server registered for extension `{ext}` (file `{path}`)")]
    NoServerForExtension { ext: String, path: String },

    #[error("LSP server `{language}` not enabled in this binding's `lsp.languages` whitelist")]
    LanguageNotEnabled { language: LspLanguage },

    #[error("LSP request `{method}` timed out after {timeout_secs}s — server still alive")]
    RequestTimeout { method: String, timeout_secs: u64 },

    #[error("LSP server `{language}` is in `Error` state; max-restart cap hit")]
    SessionDead { language: LspLanguage },

    #[error(
        "operation `{op}` not supported by server `{language}` (capability flag missing in `initialize` response)"
    )]
    CapabilityMissing {
        op: &'static str,
        language: LspLanguage,
    },

    #[error("LSP wire error: {0}")]
    Wire(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl LspError {
    /// Stable kind discriminator for tool-result JSON. Used by the
    /// `LspTool` handler so the model can branch on `kind` instead of
    /// pattern-matching the message string.
    pub fn kind(&self) -> &'static str {
        match self {
            LspError::ServerUnavailable { .. } => "ServerUnavailable",
            LspError::FileTooLarge { .. } => "FileTooLarge",
            LspError::FileNotFound { .. } => "FileNotFound",
            LspError::NotAFile { .. } => "NotAFile",
            LspError::NoServerForExtension { .. } => "NoServerForExtension",
            LspError::LanguageNotEnabled { .. } => "LanguageNotEnabled",
            LspError::RequestTimeout { .. } => "RequestTimeout",
            LspError::SessionDead { .. } => "SessionDead",
            LspError::CapabilityMissing { .. } => "CapabilityMissing",
            LspError::Wire(_) => "Wire",
            LspError::Other(_) => "Other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_unavailable_display_includes_install_hint() {
        let err = LspError::ServerUnavailable {
            language: LspLanguage::Rust,
            binary: "rust-analyzer",
            install_hint: "rustup component add rust-analyzer",
        };
        let msg = err.to_string();
        assert!(msg.contains("rust-analyzer"));
        assert!(msg.contains("rustup component add rust-analyzer"));
        assert!(msg.contains("rust"));
    }

    #[test]
    fn kinds_are_distinct() {
        let samples: Vec<LspError> = vec![
            LspError::ServerUnavailable {
                language: LspLanguage::Rust,
                binary: "x",
                install_hint: "y",
            },
            LspError::FileTooLarge {
                path: "x".into(),
                max_bytes: 0,
                actual_bytes: 0,
            },
            LspError::FileNotFound { path: "x".into() },
            LspError::NotAFile { path: "x".into() },
            LspError::NoServerForExtension {
                ext: ".x".into(),
                path: "x".into(),
            },
            LspError::LanguageNotEnabled {
                language: LspLanguage::Rust,
            },
            LspError::RequestTimeout {
                method: "x".into(),
                timeout_secs: 30,
            },
            LspError::SessionDead {
                language: LspLanguage::Rust,
            },
            LspError::CapabilityMissing {
                op: "x",
                language: LspLanguage::Rust,
            },
            LspError::Wire("x".into()),
        ];
        let mut kinds = std::collections::HashSet::new();
        for e in &samples {
            assert!(kinds.insert(e.kind()), "duplicate kind `{}`", e.kind());
        }
        assert_eq!(kinds.len(), samples.len());
    }
}
