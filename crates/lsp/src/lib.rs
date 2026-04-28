//! Phase 79.5 — `nexo-lsp` crate.
//!
//! Hand-rolled Language Server Protocol *client* for in-process
//! invocation by the agent runtime. Wraps four built-in servers
//! (rust-analyzer / pylsp / typescript-language-server / gopls)
//! behind a single [`LspManager`] and a 5-op tool surface
//! (`go_to_def`, `hover`, `references`, `workspace_symbol`,
//! `diagnostics`).
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/LSPTool/` — schema, prompt,
//!     handler, formatters, file-size cap.
//!   * `claude-code-leak/src/services/lsp/` — `LSPClient`,
//!     `LSPServerInstance` (FSM, max-restart cap),
//!     `LSPServerManager` (extension routing).
//!
//! Reference (secondary):
//!   * `research/src/agents/pi-bundle-lsp-runtime.ts:1-398` —
//!     OpenClaw's hand-rolled JSON-RPC framing reference.
//!
//! Why hand-roll instead of `async-lsp = "0.2.4"`: that crate is
//! oriented toward *building* LSP servers (tower middleware
//! around `MainLoop::new_server`); the client-side path is
//! sparsely documented. ~150 LoC of `Content-Length: N\r\n\r\n`
//! codec + JSON-RPC routing keeps the deps small and avoids
//! fighting the framework.

pub mod client;
pub mod codec;
pub mod error;
pub mod format;
pub mod launcher;
pub mod manager;
pub mod session;
pub mod types;

pub use client::{DiagnosticsCache, FileDiagnostics, LspClient};
pub use error::LspError;
pub use launcher::LspLauncher;
pub use manager::{boot, ExecutePolicy, LspManager};
pub use session::{session_from_launcher, LspSession, SessionConfig};
pub use types::{
    language_for_extension, matrix_entry, LspLanguage, LspRequest, LspToolOutput,
    ResolvedCapabilities, ServerStatus, SessionState, LANGUAGE_MATRIX,
};

// Re-exports activated as each module is implemented:
// pub use manager::LspManager;
// pub use session::{LspSession, SessionConfig};
