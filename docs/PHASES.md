# Documentation phases

Phase-by-phase plan for writing the nexo-rs mdBook. Each phase maps to
one or more code phases from `../PHASES.md`, plus cross-cutting topics
(operations, CLI, config).

The goal: capture **as much as possible** about what was built, why
decisions were made, and how things actually work — not just API
signatures. Every phase includes a "deep-dive" checklist that pushes
beyond surface docs.

Legend: `⬜ pending  🔄 in progress  ✅ done`

Progress: **5 / 18 phases done, 0 in progress**

---

## Phase D0 — Foundation   🔄

Boilerplate so the book compiles and publishes.

- [x] `book.toml` + language=en + mermaid preprocessor
- [x] `SUMMARY.md` full skeleton
- [x] `introduction.md` with pitch + top-level architecture diagram
- [x] `getting-started/installation.md`
- [ ] `getting-started/quickstart.md` — minimum viable agent in ≤5 min
- [ ] `getting-started/setup-wizard.md` — every wizard step + screenshot flow
- [ ] `license.md` + `contributing.md`
- [ ] GitHub Actions workflow `.github/workflows/docs.yml` — build + deploy gh-pages
- [ ] Confirm `mdbook build docs` passes with zero warnings
- [ ] Confirm gh-pages renders search + mermaid on the live site

**Done criteria:** anyone can clone, read the intro and quickstart, and
run their first agent — and the site deploys automatically.

---

## Phase D1 — Architecture   ✅

Mapping to code phases 1 (core runtime) and 9 (polish/ops).

Pages: `architecture/overview.md`, `architecture/agent-runtime.md`,
`architecture/event-bus.md`, `architecture/fault-tolerance.md`.

Deep-dive checklist:
- [x] Top-level component diagram (mermaid) — process boundaries, which
  crate owns what
- [x] Agent lifecycle sequence diagram — boot → discovery → plugin load
  → LLM loop → shutdown
- [x] EventBus contract: topic namespace table (`plugin.inbound.*`,
  `plugin.outbound.*`, `agent.route.*`, `plugin.health.*`, etc.)
- [x] SessionManager: how sessions are keyed, TTL, cleanup, resurrection
- [x] CircuitBreaker: state machine diagram (closed → open → half-open),
  thresholds per external dep, retry policy table
- [x] Disk queue behavior: where files live, format, drain order, poison
  pill handling
- [x] DLQ shape + CLI (`agent dlq list / replay / purge`)
- [x] Backpressure: thresholds, what happens when a plugin stalls
- [x] Single-instance lockfile: file layout, PID check, takeover flow
- [x] Graceful shutdown sequence: drain order, deadlines, SIGTERM path

**Done criteria:** a reader who has never seen the code can explain, on
paper, what happens from a WhatsApp message hitting the NATS topic to
the agent emitting an outbound reply.

---

## Phase D2 — Configuration   ✅

Mapping to phase 1.2 + scattered config additions.

Pages: `config/layout.md`, `config/agents.md`, `config/llm.md`,
`config/broker.md`, `config/memory.md`, `config/drop-in.md`.

Deep-dive checklist:
- [x] Full layout tree of `config/` with description per file
- [x] Env var resolution rules: `${VAR}`, `${VAR:-default}`,
  `${file:./secrets/x}`, `${file:/run/secrets/x}`, path-traversal rules
- [x] Secret precedence: env → file → docker secret → error
- [x] Full `agents.yaml` field reference (every key, every constraint)
- [x] Drop-in directory: merge order, override rules, `.example.yaml`
  convention, gitignore reasoning
- [x] `allowed_tools` semantics (build-time pruning vs runtime filter)
- [x] `inbound_bindings` and `outbound_allowlist` with examples
- [x] Per-provider LLM config schema + auth modes
- [x] Memory config: per-agent isolation, DB location, retention

**Done criteria:** every field documented with type, default, example,
and at least one "common mistake" callout.

---

## Phase D3 — LLM providers   ✅

Mapping to phase 3 + 15 (Anthropic auth).

Pages: `llm/minimax.md`, `llm/anthropic.md`, `llm/openai.md`,
`llm/retry.md`.

Deep-dive checklist:
- [x] MiniMax M2.5: why it's the primary, API key vs Token Plan OAuth,
  fallback chain, group_id handling
- [x] Anthropic: API key vs OAuth PKCE subscription, credentials reader
  paths, token refresh cadence
- [x] OpenAI-compatible: how Ollama / Groq / local proxies plug in,
  supported model name formats
- [x] Tool registry: how JSON-schema tools are passed per provider
- [x] Rate limiter: per-provider requests/sec, quota alert threshold,
  429 handling with exponential backoff + jitter
- [x] Retry policy table: 5xx attempts, 429 attempts, backoff curves
- [x] Error classification: which errors retry, which fail fast
- [x] Streaming vs non-streaming: current stance, implications

**Done criteria:** reader can plug in a new provider by following the
docs alone.

---

## Phase D4 — Plugins (channels)   ✅

Mapping to phases 4 (browser), 6 (WhatsApp), phase 13 (google plus
Telegram, Email, gmail-poller).

Pages: `plugins/whatsapp.md`, `plugins/telegram.md`, `plugins/email.md`,
`plugins/browser.md`, `plugins/google.md`.

Deep-dive checklist per plugin:
- [x] NATS topics consumed / produced (diagram)
- [x] Config schema + env requirements
- [x] Pairing / auth flow (QR, OAuth, API key)
- [x] Event shape (inbound + outbound JSON)
- [x] Tool surface exposed to the LLM (names, args, return)
- [x] Error modes + recovery
- [x] Known limitations

Specific:
- [x] WhatsApp: `whatsapp-rs` wrapper, session dir, allow-list, media
  download, message store
- [x] Telegram: bot API token, chat allow-list, reply/edit/reaction
- [x] Email: scaffolded status + redirect to gmail-poller
- [x] Browser: CDP connection, element refs, command family table,
  screenshot strategy
- [x] Google: OAuth consent flow, scopes, token refresh, generic
  google_call; gmail-poller cron, regex extraction, dispatch

**Done criteria:** each plugin has a "happy path" example and a
"common error" troubleshooting section.

---

## Phase D5 — Memory   ⬜

Mapping to phase 5.

Pages: `memory/short-term.md`, `memory/long-term.md`, `memory/vector.md`.

Deep-dive checklist:
- [ ] Memory layers diagram: STM (in-mem) → LTM (SQLite) → vector
- [ ] Schema dump for LTM tables
- [ ] Retention / pruning rules
- [ ] Vector: sqlite-vec setup, embedding model, index config
- [ ] Tool surface: `memory_store`, `memory_search`, etc. — full API
- [ ] How workspace-git fits in (phase 10.9)

**Done criteria:** reader understands when each layer is used and can
query the SQLite file by hand to inspect state.

---

## Phase D6 — Extensions   ⬜

Mapping to phase 11.

Pages: `extensions/manifest.md`, `extensions/stdio.md`,
`extensions/nats.md`, `extensions/cli.md`, `extensions/templates.md`.

Deep-dive checklist:
- [ ] Full `plugin.toml` schema — every field, defaults, validation
- [ ] Discovery: search paths, ignore dirs, allowlist/disabled,
  max_depth, follow_links, nested-prune semantics
- [ ] Gating: `requires.bins`, `requires.env`, skip-vs-fail behavior
- [ ] Stdio runtime: handshake protocol, tool listing, lifecycle, env
  injection, stderr routing
- [ ] NATS runtime: subject prefix, handshake, fan-out semantics
- [ ] Hooks: `before_message`, `after_tool_call`, etc. — table + flow
  diagram
- [ ] CLI: `agent ext list / install / uninstall / doctor`
- [ ] Watcher: file-change detection, debounce, reload semantics
- [ ] Templates: `template-rust`, `template-python` — walkthrough

**Done criteria:** a developer can ship a new extension by reading the
docs alone, with no need to grep source.

---

## Phase D7 — MCP   ⬜

Mapping to phase 12.

Pages: `mcp/introduction.md`, `mcp/client.md`, `mcp/server.md`.

Deep-dive checklist:
- [ ] What is MCP (brief) + why we implement both sides
- [ ] Client stdio: spawn model, handshake, tool/resource discovery
- [ ] Client HTTP: connection, session mgmt, auth
- [ ] Tool catalog: unified view over extensions + MCP + built-ins
- [ ] `tools/list_changed`: hot-reload flow, diff application
- [ ] Agent as MCP server: listen transport, exposed tools, resource
  endpoints
- [ ] MCP servers in extensions (phase 12.7)
- [ ] Sentinel session pattern (process-wide sharing to avoid duplicate
  MCP children)

**Done criteria:** reader can connect any MCP client (Claude Desktop,
Continue, etc.) to a running nexo-rs agent.

---

## Phase D8 — Skills & gating   ⬜

Mapping to phase 13.

Pages: `skills/catalog.md`, `skills/gating.md`.

Deep-dive checklist:
- [ ] Catalog table: all 22+ skills — name, what it does, requires,
  link to source
- [ ] `requires.env` / `requires.bins` enforcement — when tool is
  dropped vs warned
- [ ] How to add a skill as an inline extension
- [ ] Anthropic / Gemini LLM providers as "skills" (phase 13.19)
- [ ] brave-search / wolfram-alpha / docker-api / proxmox specifics

**Done criteria:** catalog is accurate; reader can predict which tools
will be available given their env.

---

## Phase D9 — TaskFlow   ⬜

Mapping to phase 14.

Pages: `taskflow/model.md`, `taskflow/manager.md`.

Deep-dive checklist:
- [ ] Schema + FlowStore persistence
- [ ] State machine diagram (pending → running → waiting → done/failed)
- [ ] FlowManager responsibilities
- [ ] `wait` / `resume` semantics, correlation ids
- [ ] Agent-facing tools: start_flow, continue_flow, get_flow_state
- [ ] Mirrored events + CLI inspection
- [ ] End-to-end example (multi-step business process)

**Done criteria:** reader can model a real 3+ step flow and wire it.

---

## Phase D10 — Soul, identity & learning   ⬜

Mapping to phase 10.

Pages: `soul/identity.md`, `soul/memory.md`, `soul/dreaming.md`.

Deep-dive checklist:
- [ ] Identity: who-am-I prompt, per-agent config
- [ ] SOUL.md: structure, write rules, read cadence
- [ ] MEMORY.md: index shape, max size, truncation
- [ ] Transcripts: format, location, rotation
- [ ] Recall signals: what triggers lookup, ranking
- [ ] Dreaming: when it runs, what it does, safety rails
- [ ] Vocabulary learning
- [ ] Self-report tool
- [ ] Workspace-git: per-agent repo, commit cadence, what gets
  committed

**Done criteria:** reader understands how an agent "learns" across
sessions.

---

## Phase D11 — Operations   ⬜

Mapping to phase 9 + scattered ops work.

Pages: `ops/docker.md`, `ops/metrics.md`, `ops/logging.md`,
`ops/dlq.md`.

Deep-dive checklist:
- [ ] Docker: compose file walkthrough, volumes, secrets, networking
- [ ] Metrics: full Prometheus metric table (name, type, labels, when
  emitted)
- [ ] Example Grafana queries
- [ ] Logging: tracing setup, log levels, structured fields, correlation
  across agents
- [ ] DLQ: when items land there, inspect / replay / purge flow
- [ ] Health endpoints: what they report, how to wire in k8s liveness /
  readiness

**Done criteria:** ops team can run this in production without reading
code.

---

## Phase D12 — CLI reference   ⬜

Pages: add `cli/reference.md` (not yet in SUMMARY).

Deep-dive checklist:
- [ ] `agent` top-level subcommands: `run`, `setup`, `dlq`, `ext`, `flow`
- [ ] Every subcommand: flags, env inputs, exit codes, examples
- [ ] Error code table (the README mentions `1 extension not found`,
  `7 uninstall missing --yes`, etc. — make it exhaustive)
- [ ] Shell completion (if any)

**Done criteria:** `agent --help` output is mirrored in docs with
expanded examples.

---

## Phase D13 — Recipes & tutorials   ⬜

Pages: add under `recipes/` (new top-level).

Ideas:
- [ ] "Build a WhatsApp sales agent like Ana"
- [ ] "Route between two agents with `agent.route.*`"
- [ ] "Write a custom extension in Python"
- [ ] "Use nexo-rs as an MCP server from Claude Desktop"
- [ ] "Run behind NATS with TLS + auth"

**Done criteria:** each recipe runs end-to-end from a clean checkout.

---

## Phase D14 — API reference bridge   ⬜

- [ ] `cargo doc --workspace --no-deps` produced in CI
- [ ] Published under `/api/` on gh-pages
- [ ] "API reference" link in mdBook top nav
- [ ] Public crate surface audited: every `pub` item has a `///` doc

**Done criteria:** mdBook for prose + rustdoc for API, both live under
the same domain.

---

## Phase D15 — Architecture Decision Records   ⬜

- [ ] Create `docs/src/adr/` with an `README.md` index
- [ ] Backfill major decisions as ADRs: NATS over RabbitMQ, sqlite-vec
  over Qdrant, workspace-git memory, per-agent sandboxing, Signal
  Protocol (via whatsapp-rs), drop-in agents directory, MCP dual role

**Done criteria:** a reader reviewing `adr/` can see every "why we
chose X over Y" decision.

---

## Phase D16 — Translations   ⬜ (optional)

- [ ] Spanish mirror under `docs/src-es/` once English stabilizes

---

## Phase D17 — Polish & launch   ⬜

- [ ] Broken-link check in CI
- [ ] Spell check (typos)
- [ ] Every page has at least one diagram or concrete example
- [ ] "Edit this page" link on every page works
- [ ] Custom favicon + social-share meta
- [ ] Add docs badge to README
