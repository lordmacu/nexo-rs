# nexo-lsp

Phase 79.5 — in-process Language Server Protocol client for Nexo.

Wraps four built-in language servers (rust-analyzer, pylsp,
typescript-language-server, gopls) behind a single `LspManager`
and exposes a 5-op LLM tool: `go_to_def`, `hover`, `references`,
`workspace_symbol`, `diagnostics`.

See `docs/src/architecture/lsp.md` for operator-facing docs and
`PHASES.md` § 79.5 for the implementation spec.
