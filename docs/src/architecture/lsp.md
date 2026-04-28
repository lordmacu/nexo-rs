# LSP tool (Phase 79.5)

The `Lsp` tool gives the model in-process access to four
Language Server Protocol servers — rust-analyzer, pylsp,
typescript-language-server, gopls — for code intelligence
queries (go-to-definition, hover, references, workspace symbol,
diagnostics) without spawning a sub-shell or shelling out to
`grep`.

## Built-in language matrix

| Language | Binary | Install hint |
|----------|--------|--------------|
| rust | `rust-analyzer` | `rustup component add rust-analyzer` |
| python | `pylsp` | `pip install python-lsp-server` |
| typescript / javascript | `typescript-language-server` | `npm i -g typescript-language-server` |
| go | `gopls` | `go install golang.org/x/tools/gopls@latest` |

At boot the daemon probes PATH for each binary. Missing binaries
get a single `tracing::warn!` line that includes the install
hint. Discovered binaries are cached for the lifetime of the
process; `nexo setup reload` re-probes if the operator installs
a missing one.

## Tool surface

A single tool, `Lsp`, dispatched by the `kind` field. All
positional fields (`line`, `character`) are **1-based** to match
editor UX — the underlying LSP wire is 0-based but the tool
description and handler perform the conversion.

```json
{ "kind": "go_to_def",        "file": "src/foo.rs", "line": 42, "character": 8 }
{ "kind": "hover",            "file": "src/foo.rs", "line": 42, "character": 8 }
{ "kind": "references",       "file": "src/foo.rs", "line": 42, "character": 8 }
{ "kind": "workspace_symbol", "query": "Foo" }
{ "kind": "diagnostics",      "file": "src/foo.rs" }
```

`workspace_symbol` accepts an empty `query` ("all symbols").
`diagnostics` returns the latest `publishDiagnostics` snapshot
for the file; if the file was just opened and no snapshot has
arrived yet, the tool waits up to 2 seconds before returning
`(no diagnostics)`.

## Per-binding policy

Opt in per agent in `agents.yaml`:

```yaml
agents:
  - id: cody
    lsp:
      enabled: true
      languages: [rust]              # whitelist; empty = all discovered
      prewarm: [rust]                # warmed at boot
      idle_teardown_secs: 600        # 10 min
```

`enabled: false` (the default) keeps the `Lsp` tool unregistered
for the agent — the model never sees it advertised.
`languages: []` (the default when `enabled: true`) permits every
discovered language; provide an explicit list to sandbox a
binding to a single language.

## Lifecycle

- **Lazy spawn** — the binary is launched on first tool call to a
  given `(workspace_root, language)` tuple. Cold start: ~500 ms
  for rust-analyzer, lower for the others.
- **Pre-warm** — listing a language under `lsp.prewarm` spawns
  it during boot so the first model call hits a warm session.
- **Idle teardown** — sessions with no model activity for
  `idle_teardown_secs` are shut down cleanly (`shutdown` →
  `exit` LSP requests, then bounded `kill_on_drop`). Phase 19/20
  poller calls do **not** count as activity, so a workspace
  with only synthetic traffic still releases the server's RAM.
- **Crash recovery** — a session that exits non-zero increments a
  restart counter (cap `max_restarts = 3`); the next call
  re-spawns. After the cap, the session reports `SessionDead`
  and never auto-restarts again.
- **`-32801 ContentModified` retries** — rust-analyzer in mid-
  index returns this code; the session retries with exponential
  backoff (500 ms / 1 s / 2 s) before surfacing the error.

## Plan-mode

All five MVP ops are read-only — `Lsp` is in `READ_ONLY_TOOLS`
so plan-mode never refuses an Lsp tool call.

## Output format

Results are formatted as `path:line:col` with workspace-relative
paths when shorter, mirroring `claude-code-leak/src/tools/LSPTool/
formatters.ts`. A label is appended where the LSP server
provides one (`"Definition: src/bar.rs:120:5 (struct Bar)"`).

Per-result cap: 100 KB total. Workspace-symbol queries that
exceed the cap are truncated with a `+N more results` note.

## Errors

The handler returns `{"ok": false, "error", "kind"}` instead of
panicking. Stable `kind` discriminators:

- `ServerUnavailable` — binary missing on PATH (error includes
  the install hint).
- `LanguageNotEnabled` — binding's `lsp.languages` whitelist
  excludes the target.
- `NoServerForExtension` — file extension not in the matrix.
- `FileNotFound` / `NotAFile` / `FileTooLarge` (10 MB cap).
- `RequestTimeout` — server stalled past 30 seconds; the session
  receives `$/cancelRequest` and stays alive.
- `SessionDead` — max-restart cap exceeded.
- `CapabilityMissing` — the running server doesn't advertise
  the `kind`'s capability (the dynamic description should hide
  this case in practice).
- `Wire` — JSON-RPC framing or argument parse error.

## References

- **PRIMARY**: `claude-code-leak/src/tools/LSPTool/` (LSPTool.ts,
  schemas.ts, formatters.ts, prompt.ts) and
  `claude-code-leak/src/services/lsp/` (LSPClient.ts,
  LSPServerInstance.ts, LSPServerManager.ts, manager.ts,
  passiveFeedback.ts).
- **SECONDARY**: `research/src/agents/pi-bundle-lsp-runtime.ts`
  (OpenClaw's hand-rolled JSON-RPC framing and capability-
  filtered tool registration).
- **Spec + plan**: `proyecto/PHASES.md::79.5`.
