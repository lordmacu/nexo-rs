# Changelog

All notable changes to this project are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project adheres to [Semantic Versioning](https://semver.org)
**once `v1.0.0` is tagged**. Until then breaking changes may land on
`main` between any two commits; see the commit history for detail.

## [Unreleased]

## [0.1.1](https://github.com/lordmacu/nexo-rs/releases/tag/nexo-rs-v0.1.1) - 2026-04-25

### Added

- *(pairing)* channel-adapter registry + per-channel reply delivery
- *(core)* wire pairing gate into runtime intake
- *(core+bin)* wire pairing — config field, policy, CLI
- *(pairing)* DM-challenge gate + setup-code crate scaffold
- *(core)* wire web_search tool + per-agent/per-binding policy
- *(web-search)* multi-provider search crate scaffold
- *(core/link-understanding)* fetch + extract URLs into prompt context
- *(boot)* wire context-optimization into AgentRuntime
- *(config)* per-agent context_optimization override
- *(poller)* kind: agent_turn — scheduled LLM turns from YAML
- *(llm)* DeepSeek connector via OpenAI-compatible reuse
- *(llm/anthropic)* cache_control breakpoints + token counter
- *(admin)* Telegram form — chat-id allowlist + auto-transcribe; Agents — credentials pinning UI [no-docs]
- *(llm/context-opt)* foundation types for prompt cache + compaction
- *(admin)* Telegram chat-ids + auto-transcribe + agent credentials pin [no-docs]
- *(admin/channels)* Telegram edit (PATCH) + delete [no-docs]
- *(poller/gmail)* retire legacy crate + ship six gmail_* LLM tools
- *(admin)* channels list + add-telegram form + hot-reload dev loop [no-docs]
- *(poller/gmail)* seen-id dedup cache + sample pollers.yaml
- *(poller-ext)* extension-shipped custom LLM tools
- *(admin/wizard)* avatar, draft persistence, getMe probe, WhatsApp reuse [no-docs]
- *(extensions)* template-poller-python — sample stdio poller in 130 LoC
- *(poller-ext)* poller capability for stdio extensions
- *(admin)* Phase A1 first-run wizard (identity, soul, brain, channel)
- *(metrics)* publish agent_poller series under /metrics
- *(gmail-poller)* legacy YAML translator, no own loop
- *(poller-tools)* LLM tools — pollers_{list,show,run,pause,resume,reset}
- *(poller)* wire runner into main.rs (boot + admin + CLI)
- *(admin)* React + Vite + Tailwind bundle served by agent admin
- *(poller)* runner core + backoff + hot-reload
- *(admin)* 'agent admin' command — cloudflared tunnel + Basic Auth
- *(poller)* scaffold + types + schedule + sqlite state
- *(google)* lazy-refresh client_id/secret on file mtime change
- *(auth)* hot-reload credentials via POST /admin/credentials/reload
- *(cli)* agent reload — trigger hot-reload via control.reload topic
- *(core)* intake reads from snapshot.load() — hot-reload now takes effect
- *(browser)* BrowserConfig.args — forward extra flags to Chrome
- *(auth)* per-(channel,instance) circuit breakers
- *(install)* Termux (Android) compatibility — additive, no breakage
- *(boot)* wire ConfigReloadCoordinator after agents spawn
- *(core)* ConfigReloadCoordinator — hot-swap of existing agents
- *(auth)* strict mode rejects legacy inline google_auth
- *(core)* ReloadCommand channel + apply handler in AgentRuntime
- *(install)* native / no-Docker install path — doc + bootstrap script
- *(core)* debounced config file watcher for Phase 18
- *(setup)* phase 17 — multi-instance WA/TG + google-auth.yaml flows
- *(core)* telemetry primitives for Phase 18 hot-reload
- *(core)* AgentRuntime owns an ArcSwap<RuntimeSnapshot>
- *(setup)* run credential gauntlet inside the wizard
- *(core)* RuntimeSnapshot — immutable per-agent reloadable state
- *(config)* RuntimeConfig schema for Phase 18 hot-reload
- *(auth)* phase 17 — runtime integration
- *(auth)* phase 17 scaffold — per-agent credential framework
- *(config)* resolve relative paths against config dir at load time
- *(core)* wire ToolRegistryCache into runtime intake
- *(boot)* validate model.provider against the LLM registry
- *(boot)* second-pass binding validation after tool registry assembly
- *(core)* aggregate binding validation + wildcard overlap warn
- *(core)* enforce per-binding allowed_tools at LLM turn + execution
- *(plugins,core)* outbound + delegation read effective policy
- *(core)* prompt, skills, and allowed_delegates read from effective policy
- *(core)* LLM model read from effective policy per binding
- *(core)* resolve EffectiveBindingPolicy at inbound intake
- *(core)* per-binding tool registry cache
- *(core)* AgentContext carries EffectiveBindingPolicy
- *(boot)* validate per-binding overrides after config load
- *(core)* binding_validate — boot-time checks for per-binding overrides
- *(core)* EffectiveBindingPolicy — resolve per-binding overrides
- *(config)* binding overrides — Option<> fields on InboundBinding
- *(plugins)* gmail-poller — cron-style email → broker bridge
- *(config)* load private agents from config/agents.d/*.yaml
- Ana sales agent + per-agent outbound allowlist + setup polish
- *(setup)* guided wizard + google plugin extraction + inline pairing
- agent framework phases 1-14 — runtime, memory, LLMs, plugins, skills, taskflow
- *(config)* per-agent isolation fields + multi-instance plugin shapes
- *(1.2)* config loading — AppConfig::load, env var resolution, typed structs
- *(1.1)* workspace scaffold — 9 crates, config YAMLs, cargo build clean

### Fixed

- *(ci)* cross-arm64 — install libssl-dev:arm64 + reqwest rustls
- *(ci)* rustfmt one-liner + sort_by_key for clippy 1.95
- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain
- *(audit)* land 18 of 25 findings from AUDIT-2026-04-25-pass2
- *(audit)* land 16 of 36 findings from AUDIT-2026-04-25
- *(core)* hot-reload runs post-assembly tool-name validation
- *(cli)* bring BrokerHandle trait into scope + derive Deserialize on ReloadOutcome
- *(auth)* redact inline credential paths in error output
- *(core)* make ToolRegistryCache::get_or_build atomic + review follow-ups

### Other

- *(claude)* bump deferred sub-phase counter (148 / 6 deferred)
- *(release)* bump nexo-pairing + nexo-memory to 0.1.2; sync path-dep pins
- *(release)* per-crate independent versioning
- *(ci)* release-plz workflow + config for auto-publish
- *(release)* add check-registry.sh
- *(release)* publish-order script + RELEASE.md
- *(PHASES)* expand Phase 27 release pipeline into sub-phases
- *(release)* bump workspace 0.1.0 → 0.1.1, add per-crate READMEs
- telemetry counters + histogram (W-1)
- *(PHASES)* rename stale `agent_llm::TokenCounter` ref to `nexo_llm::`
- link understanding telemetry (L-1)
- align stale agent-* prose to nexo-* naming
- agent_* crates → nexo_*, agent bin → nexo
- hot-reload context_optimization flags via per-turn snapshot read
- *(context-opt)* operations guide + admin-ui tech-debt entry
- *(admin-ui/PHASES)* expand A6 — MCP gaps + manager UI [no-docs]
- roadmap Phases 20-26 — agent_turn done, OpenClaw-parity backlog
- *(followups)* document Phase 19 V2 deferrals
- *(poller)* config/pollers.md + build-a-poller.md, phase index
- Cargo.lock sync after rust-embed + sha2 pulls [no-docs]
- *(config)* document per-agent + per-binding output language
- *(hot-reload)* security model section after the cross-phase audit
- *(followups)* document agent-llm + agent-mcp telemetry parallel-test races
- phase 17 follow-up surfaces (hot-reload, breakers, device-code, lazy-refresh, multi-instance setup)
- *(recipes)* hot-reload — rotate API key, A/B prompts, narrow allowlist
- *(followups)* close items 4 + 6 (strict legacy hard-error, inline path display)
- *(followups)* close item 3 (google device-code OAuth en setup)
- *(release)* cargo fetch + --frozen instead of --locked [no-docs]
- *(followups)* close item 5 (google lazy-refresh)
- commit Cargo.lock (nexo-rs is a binary app) [no-docs]
- aarch64 portability check + pre-built release binaries [no-docs]
- *(followups)* close item 2 (hot-reload credentials)
- CHANGELOG + dependabot [no-docs]
- Phase 18 config hot-reload — 9/9 sub-phases complete
- OSS repo chrome — SECURITY, CoC, issue + PR templates [no-docs]
- pre-commit docs-sync gate
- *(followups)* close phase 17 setup wizard items (multi-instance, credentials autowrite, google store migration)
- enable GitHub icon link in mdBook top-right
- *(followups)* expand phase 17 setup wizard deferred items
- prominent docs link + deep links to top pages
- *(docs)* make rustdoc non-fatal so mdBook still deploys
- *(phase 17)* sync plugin + metrics + agents pages to credentials
- *(core)* add arc-swap + notify deps for Phase 18 hot-reload
- *(d17)* polish — link-check in CI, README badges
- *(d15)* architecture decision records — nine backfilled ADRs
- *(followups)* phase 17 deferred items
- *(d14)* API reference bridge — rustdoc under /api/
- *(d13)* recipes — five end-to-end walkthroughs
- *(d12)* CLI reference — one page, every subcommand
- *(d11)* operations — docker, metrics + health, logging, dlq
- Option<usize> sentinel fixed; trim per-binding follow-ups to the 3 structural items remaining
- *(core)* binding_index is Option<usize>, not usize::MAX sentinel
- *(d10)* soul, identity & learning — identity, memory, dreaming
- *(d9)* TaskFlow — model + manager
- *(config)* lock down path canonicalisation + strip leading ./
- *(d8)* skills — catalog + gating
- ToolRegistryCache is now wired; drop it from open follow-ups
- *(d7)* MCP — introduction, client, server
- *(d6)* extensions — manifest, stdio, nats, cli, templates
- close resolved per-binding follow-ups, trim to open polish items
- *(d5)* memory — short-term, long-term, vector
- *(d4)* channel plugins — whatsapp, telegram, email, browser, google
- Phase 16 per-binding capability override — PHASES, index, user-facing
- *(d3)* LLM providers — minimax, anthropic, openai-compat, retry
- *(core)* lock down match_binding_index first-match semantics
- pre-resolve policies + skip boot prune for bound agents
- *(d2)* configuration — layout, agents, llm, broker, memory, drop-in
- *(d1)* architecture section — overview, runtime, bus, fault tolerance
- scaffold mdBook with phased doc plan + phase D0 content
- add NOTICE for mandatory attribution
- *(config)* ana.per-binding.example.yaml — two-binding override example
- *(config)* binding override YAML parse coverage
- pre-release prep — CI, dual license, ext gating, Ana→MiniMax
- README + MIT license
- *(whatsapp)* pull wa-agent 0.1.1 from crates.io
- mark 1.1 done, update progress 1/68
- mandate brainstorm before every sub-phase
- agent framework design, phases, and dev tooling

### Added

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
