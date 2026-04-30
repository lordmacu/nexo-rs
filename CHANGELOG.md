# Changelog

All notable changes to this project are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project adheres to [Semantic Versioning](https://semver.org)
**once `v1.0.0` is tagged**. Until then breaking changes may land on
`main` between any two commits; see the commit history for detail.

## [Unreleased]

### Added

- Phase 80.18 — `crates/agent-registry::dream_run` audit-log store
  for forked memory-consolidation runs. Verbatim port of leak
  `claude-code-leak/src/tasks/DreamTask/DreamTask.ts:1-158`. Mirrors
  Phase 72 turn-log pattern: `DreamRunStore` trait + `SqliteDreamRunStore`
  impl, schema migration v4 idempotent + 3 indexes, MAX_TURNS=30
  server-side cap, TAIL_HARD_CAP=1000 defends `tail(usize::MAX)`,
  JSON columns for `files_touched` + `turns` avoid join tables.
  `Option<i64>` for `prior_mtime_ms` distinguishes `Some(0)` (no prior
  consolidation file marker for autoDream) from `None` (non-lock-holding
  forks like AWAY_SUMMARY 80.14). `fork_label: String` flexible —
  supports autoDream + AWAY_SUMMARY + future Phase 51 eval forks.
  Provider-agnostic: `DreamTurn { text, tool_use_count }` plain Rust,
  no `LlmClient` coupling. 26 unit tests including idempotent insert
  on (goal_id, started_at), trim cap proof (insert 35 → final 30),
  reattach `Running → LostOnRestart` flip, drop_for_goal isolation,
  prior_mtime zero-vs-none round-trip. Phase 71 reattach integration
  deferred to 80.18.b follow-up.
- Phase 80.20 — `crates/fork::AutoMemFilter` tool whitelist for
  forked memory-consolidation work. Verbatim port of leak
  `claude-code-leak/src/services/extractMemories/extractMemories.ts:165-222`
  (`createAutoMemCanUseTool`). Allows `REPL` (cache-key parity per
  leak `:171-180`), `FileRead`/`Glob`/`Grep` (read-only), `Bash` only
  when `nexo_driver_permission::is_read_only(command)` returns true,
  `FileEdit`/`FileWrite` only when `file_path` (post-canonicalize)
  starts with the filter's `memory_dir`. Path canonicalize at
  construction + per-call defeats symlink swaps and `..` traversal.
  New helper `nexo_driver_permission::bash_destructive::is_read_only`
  composes Phase 77.8/77.9 classifiers + a positive whitelist of ~45
  read-only utilities; intentionally drops `tee`/`awk`/`perl`/`python`/
  `node`/`ruby` from the whitelist because they can shell out via
  `system(...)`. Provider-agnostic — operates on tool name + JSON
  args; expects flat top-level args (provider clients unwrap nested
  envelopes before dispatch). 24 new unit tests in `auto_mem_filter`
  + 19 new in `bash_destructive::is_read_only`. Decisions D-1+R3 in
  `proyecto/design-kairos-port.md` (conservative whitelist, fail-fast
  on missing dir, defense-in-depth via post-fork audit in 80.1).
  Consumed by Phase 80.1 autoDream + 80.14 AWAY_SUMMARY + future
  Phase 51 eval harness.
- Phase 80.19 — `crates/fork/` fork-with-cache-share subagent
  infrastructure (KAIROS port). Standalone in-process turn loop
  using `nexo_llm::LlmClient` directly (NOT Phase 67's heavyweight
  `DriverOrchestrator`, which spawns `claude` subprocesses).
  `CacheSafeParams::from_parent_request(&ChatRequest)` snapshots
  parent prompt + tools + model + message prefix; preserves any
  incomplete `tool_use` blocks bit-for-bit (leak invariant
  `forkedAgent.ts:522-525`). `DelegateMode::{Sync,ForkAndForget}`,
  `ForkHandle::take_completion()` + `Drop`-cancels-abort prevents
  leaked tokio tasks on abandoned ForkAndForget handles. `OnMessage`
  trait with `NoopCollector` / `LoggingCollector` / panic-safe
  `ChainCollector`. `ToolFilter` trait + `AllowAllFilter` default
  (Phase 80.20 ships `AutoMemFilter`). `tracing` span
  `fork.subagent` with run_id + cache_key_hash + mode; inline
  `WARN fork.cache_break_detected` when first-turn cache hit ratio
  drops below 0.5 (Phase 77.4 heuristic). 42 unit tests pass on
  `cargo test -p nexo-fork`. Decisions D-8 in
  `proyecto/design-kairos-port.md`. Consumed by Phase 80.1
  autoDream + 80.14 AWAY_SUMMARY + future Phase 51 eval harness.
  Refactor of `delegation_tool.rs` to consume the new infra is
  follow-up 80.19.b (out of scope for 80.19 itself).
- Phase 27.1 — `cargo-dist` baseline. `dist-workspace.toml` declares
  the cross-target matrix (`x86_64-unknown-linux-gnu` host fallback +
  `x86_64`/`aarch64-unknown-linux-musl` + `x86_64`/`aarch64-apple-darwin`
  + `x86_64-pc-windows-msvc`). `make dist-check` runs the local smoke
  gate (`scripts/release-check.sh`) over whatever `dist build`
  produced, validating tarball contents + sha256 + host-native
  `--version`. `nexo version` (or `nexo --version --verbose`) prints
  build provenance — git-sha, target triple, build channel, build
  timestamp — captured at compile time by `build.rs` and consumed
  via `env!("NEXO_BUILD_*")`. Dev-only programs (`browser-test`,
  `integration-browser-check`, `llm_smoke`) moved to `examples/` so
  cargo-dist excludes them from release tarballs. `release-plz`
  remains the source of truth for version bumps + crates.io publish
  + per-crate `CHANGELOG.md`. Operator notes:
  [`packaging/README.md`](packaging/README.md), contributor docs:
  [Releases](docs/src/contributing/release.md).
- `agent admin` subcommand: runs a web admin UI behind a Cloudflare
  quick tunnel. Auto-installs `cloudflared` per OS/arch on first run,
  starts a loopback HTTP server, mints a fresh 24-char random
  password per launch, and prints a new `https://…trycloudflare.com`
  URL every time. HTTP Basic Auth (`admin` / `<password>`) gates
  every request. Serves the React + Vite + Tailwind bundle from
  `admin-ui/` embedded at Rust compile time via `rust-embed`. See
  [CLI reference — admin](https://lordmacu.github.io/nexo-rs/cli/reference.html#admin).
- `admin-ui/` scaffold (React 18, Vite 5, TS 5, Tailwind 3). First
  page is a minimal "hello" layout; the full admin surface (agent
  directory, DLQ, live reload, config editor) lands in follow-ups.
  `scripts/bootstrap.sh` runs `npm install && npm run build`
  automatically when `npm` is on PATH.
- Native / no-Docker install path: `docs/src/getting-started/install-native.md` +
  idempotent `scripts/bootstrap.sh` (Linux, macOS, Termux).
- Termux (Android) support:
  - Dedicated install guide `docs/src/getting-started/install-termux.md`
    with root-vs-non-root breakdown.
  - `bootstrap.sh` detects `$TERMUX_VERSION` / `$PREFIX` and branches:
    `pkg install rust`, `$PREFIX/bin`, defaults NATS to `skip` with
    a hint toward `broker.type: local`.
- `BrowserConfig.args: Vec<String>` forwards extra CLI flags to the
  spawned Chrome/Chromium (enables `--no-sandbox` etc. for Termux).
- Repository chrome: `SECURITY.md`, `CODE_OF_CONDUCT.md`,
  `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.{md,yml}`,
  `.github/PULL_REQUEST_TEMPLATE.md`.
- Documentation site (mdBook) published at
  <https://lordmacu.github.io/nexo-rs/> with every subsystem
  documented, Mermaid diagrams, 9 ADRs under `docs/src/adr/`, and
  5 end-to-end recipes under `docs/src/recipes/`.
- Pre-commit docs-sync gate in `.githooks/pre-commit` rejects
  production-file changes without accompanying `docs/` edits unless
  the commit message includes `[no-docs]`.
- CI: `.github/workflows/docs.yml` builds mdBook + rustdoc and
  deploys to GitHub Pages; broken local-link scan.

### Changed

- Dual-licensed `MIT OR Apache-2.0` with an enforceable `NOTICE`
  attribution block (ADR 0009).
- `README.md` rewritten with badges and deep links into the
  published documentation.
- **M2** — MCP audit log `tools/call` rows now record the real
  `args_hash` (sha256 truncated to 16 lowercase hex chars / 64 bits)
  and `args_size_bytes` (JSON-serialized byte length) instead of the
  placeholder `None`/`0`. Honors the existing
  `audit_log.redact_args` (default `true`),
  `audit_log.per_tool_redact_args` (per-tool override wins over
  global), and `audit_log.args_hash_max_bytes` (default 1 MiB, hard
  ceiling 16 MiB) knobs — none of those YAML keys change. New
  internal module `crates/mcp/src/server/audit_log/hash.rs` exposes
  the helpers as `pub(crate)`. `SELECT args_hash, COUNT(*) FROM
  mcp_call_audit GROUP BY args_hash` correlation queries now return
  real data; the SQLite schema is unchanged. Provider-agnostic —
  operates on the MCP wire envelope, identical regardless of which
  LLM client (Claude Desktop / Cursor / Continue / Cody / Aider) or
  backing provider (Anthropic / MiniMax / OpenAI / Gemini /
  DeepSeek / xAI / Mistral) drives the call. Truncation length
  matches the prior-art pattern in claude-code-leak
  (`src/services/mcp/utils.ts:157-168` `hashMcpConfig` + 4 other
  sites all `slice(0, 16)`).
- **C3** — `crates/setup/src/capabilities.rs::INVENTORY` extended
  from 9 → 12 entries closing the audit drift. New entries:
  `CHAT_AUTH_SKIP_PERM_CHECK` (auth-wide file-perm-gauntlet bypass,
  High), `NEXO_CLAUDE_CLI_VERSION` (Anthropic OAuth Bearer CLI
  version stamp override, Low), and `config-self-edit` Cargo feature
  (gates the self-config-editing ConfigTool, Critical). New
  `ToggleKind::CargoFeature(&'static str)` variant supports
  compile-time gates alongside runtime env-var toggles. Module
  doc-comment expanded with provider-agnostic clause: every LLM
  provider (Anthropic, MiniMax, OpenAI, Gemini, DeepSeek, xAI,
  Mistral, future) gets its own entries when it introduces a
  dangerous toggle (insecure-tls, skip-ratelimit, allow-write); the
  `extension` field already accepted any identifier. New regex-based
  drift-prevention test (`inventory_covers_known_dangerous_envs`)
  walks `crates/**/*.rs` and fails when an `env::var("X")` literal
  is neither in INVENTORY nor in `NON_DANGEROUS_ENV_ALLOWLIST` —
  surfaced 13 previously-unclassified env reads (all benign,
  classified into the allowlist with category comments). Implementation
  100% Rust (`cfg!`, const slice, `walkdir + regex` dev-deps); the TS
  references (claude-code-leak `envUtils.ts` + `commands/doctor/`,
  OpenClaw `auth-profiles/doctor.ts`) guided pattern, not code.
- **C2** — Hot-reload now picks up per-binding policy overrides for
  `lsp.languages` / `lsp.idle_teardown_secs`, `team.max_*` /
  `team.worktree_per_member`, `repl.allowed_runtimes`, and the
  C1-added inheritance for the four resolved fields. Tool handlers
  (`LspTool`, `ReplTool`, `TeamCreateTool`/`TeamDeleteTool`/
  `TeamSendMessageTool`/`TeamListTool`/`TeamStatusTool`) read
  policy from `ctx.effective_policy().<x>` per call instead of
  capturing it at `Tool::new`. Reload semantics: a snapshot swap
  via `ConfigReloadCoordinator` is observed on the very next
  intake event without restart. **Boolean enable flips** (e.g.
  `lsp.enabled: false → true`) still require restart — see
  `docs/src/ops/hot-reload.md::What's reloaded` for the full
  matrix. Subsystem actor lifecycle (LspManager child processes,
  ReplRegistry subprocess pool, TeamMessageRouter broker subs)
  is unchanged across reload, matching the prior-art pattern
  from claude-code-leak's MCP `useManageMCPConnections` invalidate-
  and-refetch. Implementation is 100% Rust idiomatic
  (`Arc<EffectiveBindingPolicy>` lookups, `ArcSwap<RuntimeSnapshot>`
  swap, `From` trait adapters); the TS references guided the
  pattern, not the code. Two follow-ups tracked in `FOLLOWUPS.md`:
  H-3.b (M5 — `cron_tool_bindings` registry captured at boot) and
  H-3.c (M11 — full ConfigTool config-pull at handler entry).
- **C1** — `EffectiveBindingPolicy` now resolves four additional
  per-binding overrides (`lsp`, `team`, `config_tool`, `repl`) using
  the same replace-whole strategy as `proactive` / `remote_triggers`.
  **Behavioural change**: configs that already declared
  `inbound_bindings[].repl: { ... }` will start applying it — the
  override field had been declared in Phase 79.12 but the resolver
  was missing, silently inheriting the agent-level value. Three new
  optional fields (`lsp`, `team`, `config_tool`) added to
  `InboundBinding`; defaults inherit, so pre-existing YAML is
  unaffected. `binding_validate::has_any_override` extended to
  count the seven previously-uncounted overrides
  (`plan_mode` / `role` / `proactive` / `repl` / `lsp` / `team` /
  `config_tool`); this fixes the misleading `binding without
  overrides` warning. The actual consumption of the new resolved
  fields by tool-registration paths in `src/main.rs` remains
  pinned to boot — runtime hot-reload of these policies is
  tracked under C2.

### Deprecated

_Nothing yet._

### Removed

_Nothing yet._

### Fixed

- Setup wizard no longer hardcodes a shared `whatsapp.session_dir`
  — the writer derives a per-agent path when the YAML field is
  empty, avoiding cross-agent session collisions.
- Extension tools are gated on `Requires::missing()`: if declared
  `bins` / `env` aren't available, the extension is skipped with a
  warn log instead of registering tools that fail every call.

### Security

- `SECURITY.md` formalizes the private disclosure channel
  (<informacion@cristiangarcia.co>) and sets expected response SLAs.

---

## [0.1.0] — 2026-04-24 (initial public release)

First public cut of the codebase. All 16 internal development
phases complete (120/120 sub-phases in `PHASES.md`). No backward-
compatibility commitments yet — treat the public surface as unstable
until `v1.0.0`.

<!-- Link definitions:
[Unreleased]: https://github.com/lordmacu/nexo-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lordmacu/nexo-rs/releases/tag/v0.1.0
-->
