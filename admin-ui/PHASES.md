# Admin UI — phases

Phase-by-phase plan for the `agent admin` web UI. Every feature ever
built into the agent that needs human administration **must** land
a line here before it ships — or a checkbox in an existing phase.
See `proyecto/CLAUDE.md` / auto-memory "Admin needs everything
administrable" for the rule.

Legend: `⬜ pending  🔄 in progress  ✅ done  ⏸ deferred`

---

## Universal acceptance criteria

Applies to every page in every phase below.

- [ ] **Mobile-first.** Renders cleanly at 375 × 667; tap targets
  ≥ 44 px. Tailwind responsive breakpoints scale up from there.
- [ ] **No horizontal scroll** on the main container.
- [ ] **Dark-mode aware** (Tailwind `dark:` classes respect
  `prefers-color-scheme`).
- [ ] **Keyboard operable** (tab order, visible focus ring, Esc
  closes modals).
- [ ] **Loading + empty + error states** for every data-fetching
  screen.

---

## Phase A0 — Shell   🔄

The minimum boot: tunnel + React bundle + login. Already partly
done.

- [x] `agent admin` subcommand boots cloudflared + loopback HTTP
- [x] Random password per launch printed to stdout
- [x] Signed session cookie (HMAC-SHA256, 24 h TTL, rotates every
  daemon restart) — no Basic Auth popup
- [x] `/login` HTML form with `POST /api/login` + `POST /api/logout`
- [x] React 18 + Vite + TS + Tailwind scaffold in `admin-ui/`
- [x] `rust-embed`-baked bundle, SPA-router-aware path resolution
- [ ] **Bundle-missing fallback** auto-triggers the npm build if
  `npm` is available, before falling back to the placeholder HTML
- [ ] **Dark-mode toggle** with persisted preference
- [ ] **Session keep-alive ping** from the SPA so idle tabs don't
  expire silently

---

## Phase A1 — First-run wizard   ✅

The moment the operator logs in for the first time and no agents
exist, route them into a guided wizard. Four steps; every field has
a sane default so "Next Next Next Finish" produces a working agent.

- [x] Wizard detection — `GET /api/bootstrap` returns
  `{ needs_wizard: bool, agent_count }` based on whether
  `agents.yaml` + the drop-in directory resolve to zero live
  entries
- [x] **Step 1 — Identity**: name, emoji, vibe. Defaults (`Kate` +
  `🐙` + placeholder vibe) so the screen is never blank
- [x] **Step 2 — Soul** (persona / long-form prompt): `SOUL.md`
  textarea pre-filled with a curated starter the operator can edit
- [x] **Step 3 — Brain**: picker for MiniMax / Anthropic /
  OpenAI-compat / Gemini with minimum fields; keys land under
  `./secrets/<provider>_api_key.txt` at mode 0600
- [x] **Step 4 — Channel**: Skip / Telegram (bot token) /
  WhatsApp (pair-later prompt). Writes `config/plugins/telegram.yaml`
  if absent; WhatsApp pairing still runs through `agent setup
  whatsapp` on the host
- [x] Finish button writes `config/agents.d/<slug>.yaml` +
  `IDENTITY.md` + `SOUL.md` + telegram.yaml (if absent) in one
  call to `POST /api/bootstrap/finish` and bounces into the
  Dashboard
- [x] Avatar URL field + avatar preview (circular, falls back to
  emoji on load failure)
- [x] Wizard is idempotent — localStorage-backed draft survives
  tab close / browser restart; "Start over" button clears it and
  resets to defaults
- [x] Live validation: probe Telegram token via `getMe` before
  accepting it (direct SPA → `api.telegram.org` probe, shows bot
  username + first name on success)
- [x] "Use an existing WhatsApp session" shortcut — checkbox that
  skips the "run agent setup whatsapp" prompt and assumes the
  paired session dir is already on disk

---

## Phase A2 — Channel configuration   ⬜

Top-level "Channels" section — a card per channel instance with
inline edit + pairing flow. Emphasis on getting the operator from
"I want a Telegram bot" to "my agent is receiving messages" in one
page.

### Telegram

- [x] **New Telegram channel** (`POST /api/channels/telegram`):
  instance label, token, allow_agents. `getMe` probe validates the
  token before save.
- [x] **Delete channel** (`DELETE /api/channels/<plugin>/<instance>`)
  drops the YAML entry. Secret token file stays on disk so
  re-adding the same instance skips re-pasting the token.
- [x] **Edit channel** (`PATCH /api/channels/telegram/<instance>`)
  rotates token and/or swaps `allow_agents` in place; empty
  token preserves the current `${file:...}` reference.
- [x] Chat-id allowlist editor (`allowlist.chat_ids`) — textarea in
  the Edit form, comma/newline-separated ints
- [x] Auto-transcribe toggle for voice messages with command +
  language sub-fields (shown when the checkbox is on)
- [x] Per-agent `credentials.telegram` pinning — dropdown panel on
  each agent card backed by `PATCH /api/agents/<id>/credentials`.
  Same panel handles `credentials.whatsapp` + `credentials.google`
- [ ] Live status card: polling cadence, last update offset, last
  error, bridge timeouts in the past hour
- [ ] **Pairing test**: "Send /ping to @yourbot" → UI waits for the
  webhook and flashes green

### WhatsApp

- [ ] **New WhatsApp channel** → step-by-step:
  1. Pick instance label (default `<agent>_wa`)
  2. Select `allow_agents`
  3. Click "Show pairing QR" — server renders the Unicode QR as
     SVG/PNG for the browser, not just the terminal
  4. Tailored prompt for archive / mute rules (maps to
     `behavior.ignore_chat_meta`)
- [ ] Session status: paired ✅ / waiting for scan ⏳ /
  credentials_expired ❌, last connected at
- [ ] **Re-pair** button (equivalent to the CLI `setup wipe`)
- [ ] Media dir picker with free-space probe

### Browser (CDP)

- [ ] Dropdown: spawn new Chrome vs. attach to `cdp_url`
- [ ] Chrome flags editor (pre-filled with Termux-friendly
  defaults when host is Termux)
- [ ] Live stats: open pages, CDP commands/sec

### Google (Gmail/Calendar/Drive)

- [ ] Paste `client_id` + `client_secret`
- [ ] Scope multi-select (default `gmail.modify`)
- [ ] "Run device-code flow" → shows the verification URL + user
  code in a card; polls until approval
- [ ] `gmail-poller` job builder: regex extract fields, template
  preview, forward target dropdown (recipient instance resolved
  from existing channels)

### Email (SMTP/IMAP)

- [ ] Scaffold only; mirrors the config schema but greyed-out
  while phase 17 of the backend is pending

---

## Phase A3 — Agent configuration   ⬜

Editing everything about an agent after the wizard. `agents.yaml`
and the matching `agents.d/*.yaml` are the source of truth; the UI
is a structured editor with validation.

- [ ] Agent directory list: id, emoji, model, channels, sessions
  last 24 h. Click → detail page
- [ ] **Identity tab**: structured form over IDENTITY.md
- [ ] **Soul tab**: markdown editor for SOUL.md with live preview;
  character count against the 12 KB cap
- [ ] **Brain tab**: LLM provider + model swap, per-binding
  overrides, retry / rate-limit editor
- [ ] **Tools tab**: allowed-tools checklist with globs, live
  preview of the registry that would reach the LLM
- [ ] **Channels tab**: link existing channels to this agent via
  `credentials:` block; inline creation shortcut points back to
  Phase A2
- [ ] **Memory tab**: short-term TTL / long-term DB path / vector
  toggle; "Export memory" dump as SQLite + MEMORY.md
- [ ] **Skills tab**: toggle shipped skills; inline docs per skill
  linking back to `docs/src/skills/catalog.md`
- [ ] **Delegation tab**: `allowed_delegates` + `accept_delegates_from`
  pickers; ASCII diagram of who can talk to whom
- [ ] **Dreaming tab**: enable/disable; weight sliders for the
  five factors; dry-run button ("what would promote now?")
- [ ] **Workspace tab**: file browser over the agent's workspace
  dir; read-only by default, flip to edit on demand
- [ ] **Danger zone**: pause agent (stop intake), drop session
  cache, delete agent

---

## Phase A4 — Runtime dashboard   ⬜

Live state — what's happening right now.

- [ ] Agent cards with throughput + error rate + circuit breaker
  state per provider
- [ ] DLQ surface: inspect / replay / purge (mirrors
  `agent dlq` subcommand) with bulk actions
- [ ] TaskFlow explorer: live flow list, step timeline, manual
  resume for `Manual` / `ExternalEvent` waits
- [ ] Logs tail (SSE stream from the running process) with level
  filter and structured-field search
- [ ] Metrics: embed `/metrics` Prometheus output as Grafana-style
  tiles (LLM latency p50/p95/p99, tool error rate, circuit
  breaker history)

---

## Phase A5 — Config file editor   ⬜

Power-user escape hatch for fields the structured UI doesn't
expose yet.

- [ ] Per-file YAML editor with schema-aware linting and preview
- [ ] Diff against current on disk; confirm-to-save
- [ ] One-click hot-reload (`POST /api/reload`) with per-agent ack
  list

---

## Phase A6 — Extensions / skills / MCP   🔄

### Extensions

- [ ] Extension discovery mirror: installed / disabled / invalid
- [ ] `agent ext install / uninstall / doctor --runtime` wrapped
  as UI actions

### MCP — admin surface

- [x] **MCP server manager** (CRUD on `config/mcp.yaml`):
  - [x] List: every server with transport (stdio/http), command/url,
    headers, log_level, context_passthrough override
  - [x] Add: form with stdio command+args+env or http url+headers
  - [x] Edit: rotate token / swap args, same form pre-filled
  - [x] Delete: drop entry from `mcp.servers`
  - [ ] Header value redaction in list view (Bearer / Authorization)
  - [ ] Live status pill (handshake ok / failed / spawning) — gated
    on the "Live status of MCP children" daemon work below
  - [ ] Tool count per server — gated on tool catalog telemetry
  - [ ] Hot-reload trigger via `POST /api/reload` (today an operator
    must restart the daemon or set `mcp.watch.enabled: true`)
- [ ] `agent mcp-server` on/off toggle (writes
  `config/mcp_server.yaml::enabled`) with live link + copy snippet
  for Claude Desktop JSON config (same shape doc'd in
  `docs/src/recipes/mcp-from-claude-desktop.md`)
- [ ] Allowlist editor for the agent-as-server `tools` allowlist
  with glob preview ("these tools will reach Claude Desktop")
- [x] **Exposable catalog (Phase 79.M)** — `mcp_server.expose_tools`
  is now backed by `EXPOSABLE_TOOLS` (slice in `nexo-config`) +
  per-tool boot dispatcher in `nexo-core::agent::mcp_server_bridge`.
  18 entries categorised by `SecurityTier` + `BootKind`. Admin can
  surface a curated picker (read tier label) without reinventing
  the catalog. Telemetry counters
  `mcp_server_tool_registered_total{name,tier}` +
  `mcp_server_tool_skipped_total{name,reason}` available for the
  admin status pane.

### MCP — protocol gaps the daemon owes

These all live in `crates/mcp/` and gate features the admin can
expose. Tracked here because the admin checkbox can't fire until
the daemon side lands.

- [ ] **MCP server over HTTP** — currently `agent mcp-server` is
  stdio-only. HTTP transport (cookie-auth same as admin, `/mcp/rpc`
  endpoint) unlocks remote IDE consumption (Cursor remote, web
  playgrounds, anything that can't spawn a subprocess). Highest
  value gap.
- [ ] **MCP completions** (`completion/complete`): some servers
  publish argument autocomplete; without it the LLM invents arg
  values. Cheap fix on the client side.
- [ ] **MCP resources injected into the prompt** (or surfaced as a
  pickable list): today the agent can call `resources/read` if it
  asks, but it doesn't know what's there unless the LLM proactively
  calls `resources/list`. UX decision needed (anchor docs vs auto-
  fetch vs tool wrapper).
- [ ] **MCP prompts** (`prompts/list`, `prompts/get`) — server-
  published prompt templates. Today ignored entirely. Useful for
  "summarize this email thread" reusable templates.
- [ ] **Progress notifications** — long tools emit progress %; we
  drop them. Cheap to plumb to tracing + the admin live log tail.
- [ ] **Cancellation** (JSON-RPC `$/cancelRequest`) — hung tool
  calls wait for `call_timeout` instead of being cancelled. Cheap.
- [ ] **Logging receiver** (`notifications/logging/message`) —
  forward server-side log lines to our tracing pipeline. Trivial.
- [ ] **Live status of MCP children** — every spawned server's
  handshake state, last call latency, last error. Backs the
  "MCP server manager" status pill.
- [ ] **Sampling reverso** (server-initiated LLM calls) — pinned
  off by config. Niche but specced; flip on with per-server
  caps to avoid runaway loops.
- [ ] **MCP roots** advertisement on the server side — niche.
- [ ] **Subscribe to resources** (`resources/subscribe`) — file
  watchers, DB triggers; medium effort, niche use.

### MCP server hardening (Phase 76)

Surfaces the new server-side runtime so operators can manage
third-party MCP plugins (e.g. `nexo-marketing`) from the admin UI
the moment Phase 76 lands.

- [ ] **HTTP transport endpoint editor (76.1)** — list of
  bound listeners (`stdio` vs `http://addr:port`), edit form to
  add/remove HTTP listeners and bind addresses, badge per
  listener showing live socket state.
- [ ] **Auth configuration per listener (76.3)** — picker
  `None / StaticToken / BearerJwt / MutualTls`, JWKS URL +
  cache TTL editor for JWT, secret-redacted token vault for
  StaticToken, mTLS PEM upload + fingerprint preview.
- [ ] **Tenant matrix (76.4)** — table of registered tenants
  (`tenant_id`, principal count, scopes), drill-down to view
  what tools each tenant has called in the last 24 h. Cross-
  tenant leak alerts pulled from the audit log.
- [ ] **Rate-limit editor (76.5)** — global default + per-tool
  overrides as `(rps, burst)` pairs. Live counter
  `mcp_server_rate_limit_hits_total{tenant,tool}` heatmap so the
  operator sees who is hitting the wall.
- [ ] **Backpressure dashboard (76.6)** — gauges for
  `mcp_server_in_flight{tenant}`, queue depth, per-tool timeout
  breach count; "kill all in-flight for tenant X" emergency
  button (calls into the cancellation token registry).
- [ ] **Sessions explorer (76.8)** — list active sessions
  (`session_id`, `tenant`, `principal`, idle_for, subscriptions),
  toggle for `durable_sessions`, revoke button.
- [ ] **Audit-log viewer (76.11)** — admin-side equivalent of
  `mcp_audit_tail`: filterable by tenant / tool / status /
  timeframe, export CSV. Reuses the same SQLite that the CLI
  reads so the two never disagree.
- [ ] **Live tool catalog per server (76.7)** — fed by
  `tools/list_changed` notifications (server-side now), keeps the
  "MCP server manager" tool count fresh without polling.
- [ ] **Health + readiness pills (76.10)** — `/healthz` +
  `/readyz` per listener, p99 latency sparkline pulled from
  `mcp_server_latency_seconds`.
- [ ] **TLS / reverse-proxy guidance card (76.13)** — surface
  whether the listener terminates TLS itself, sits behind a
  proxy, or is plain HTTP (with a red banner if non-loopback
  + plain HTTP + auth != `None` mismatch).
- [ ] **Extension template scaffolder (76.15)** — one-click
  "create new MCP-server extension from template" that drops
  `extensions/templates/mcp-server-skeleton/` and pre-wires
  the listener config in `mcp_server.yaml`.

---

## Phase A7 — Memory inspector   ⬜

- [ ] Per-agent memory browser (SQLite rows + workspace-git log)
- [ ] Concept-tag cloud
- [ ] Dreaming sweep history with per-run promote list
- [ ] Manual checkpoint button (`forge_memory_checkpoint`)

---

## Phase A8 — Delegation & routing visualiser   ⬜

- [ ] Live graph: agents as nodes, `agent.route.*` traffic as
  edges; colour by correlation_id
- [ ] Click an edge → open the transcript of that delegation
- [ ] Drill-down to the system prompt version that was live when
  the call fired

---

## Phase A9 — Audit & diagnostics   ⬜

- [ ] `agent --check-config` runner with pretty diff output
- [ ] Credentials gauntlet report (Phase 17 backend already
  exists)
- [ ] Startup timeline (what took how long on the last boot)
- [ ] "Snapshot everything" button — zips config, workspaces, DB
  rows (redacted), logs from the last hour for support hand-off

---

## Phase A10 — Debug / reset helpers   🔄

Developer safety valve. Absolutely **not** exposed in production
builds — gated behind a `#[cfg(debug_assertions)]` compile flag on
the Rust side and hidden in the SPA when the backend reports
`debug: false`.

- [x] **Reset button** (wizard footer + Settings → Danger zone):
  wipes agents / channel instances / credentials / plugin sessions
  / SQLite databases / transcripts / workspace dirs / DLQ. Lands
  as `POST /api/debug/reset` in Phase A0.5
- [ ] Confirm dialog with the exact file list about to be deleted
- [ ] `POST /api/debug/reset` endpoint (debug builds only):
  - Removes `config/agents.d/*.yaml` (keeps `.example.yaml`)
  - Truncates `./data/**` and `./secrets/**`
  - Clears `./data/workspace/**` (the per-agent workspaces)
  - Flushes `memory.db`, `taskflow.db`, `broker.db`
  - Responds `{ ok: true, cleared: [paths] }` on success
- [ ] Seed-data button — re-stage `config/agents.d/*.example.yaml`
  as real `.yaml` so the operator can skip the wizard during
  smoke tests
- [ ] "Fake traffic" generator: publishes synthetic
  `plugin.inbound.*` events so the dashboard has something to show
  during demos
- [ ] `GET /api/debug/env` dumps detected OS, arch, Termux flag,
  cloudflared path, bundle build ts — only in debug builds

---

## Phase A11 — Telemetry & UX polish   ⬜

- [ ] Keyboard shortcuts (`/` to focus search, `g d` for
  Dashboard, etc.)
- [ ] "Changed since you logged in" banner (new DLQ entries,
  crashed sessions)
- [ ] Page-level empty states pointing at Phase A1 wizard
- [ ] Colour-blind palette audit
- [ ] First-paint budget: Lighthouse score ≥ 95 on LAN

---

## Tech-debt registry (the "admin must follow" list)

Every new backend feature that admits a knob must add a line here
**in the same commit that ships the feature**. Blank boxes are
IOUs — features that landed in the daemon but have no UI yet.

- [ ] `credentials:` block (Phase 17) — Phase A3 agent-config tab
  Phase A3 covers this
- [ ] `NEXO_CLAUDE_CLI_VERSION` env override (Phase 15.9) — surface in
  the setup-wizard env-vars panel and the Anthropic auth tab so
  operators can patch the Claude-Code CLI version stamp without
  redeploying when Anthropic bumps the accepted version. Backend
  reads the env at request time via `claude_cli_user_agent()` in
  `crates/llm/src/anthropic_auth.rs`; default lives in
  `CLAUDE_CLI_DEFAULT_VERSION`.
- [ ] Release pipeline (Phase 27.1 `cargo-dist` baseline + 27.2
  GH Actions release workflow + 27.4 Debian/RPM packages Tier 1+3)
  — tag-to-asset pipeline complete: push `nexo-rs-v<version>` →
  2 musl tarballs (x86_64 + aarch64) + 2 `.deb` (amd64 + arm64) +
  2 `.rpm` (x86_64 + aarch64) + Termux `.deb` + sha256 sidecars
  uploaded to the GH release; smoke + install-test matrix on 5
  docker images (debian/ubuntu/fedora/rocky) validates clean
  install. Apple (`x86_64`/`aarch64-apple-darwin`) and Windows
  (`x86_64-pc-windows-msvc`) targets parked — Phase 27.6
  (Homebrew) parked with them. Admin UI tasks: surface `nexo
  --version --verbose` (git-sha, target, channel, built-at) on
  the About page once the daemon exposes those four stamps via
  `/api/info` (deferred); show per-channel quick-install snippets
  (`wget … && apt install ./*.deb`, `dnf install <url>`); add an
  "Apt repo signed badge" entry in System Status once Phase 27.4.b
  publishes the GH-Pages-hosted apt/yum repos; revive Apple/Windows
  install channel docs when their matrix entries return.
- [ ] Driver/goal monitor (Phase 67) — pending tile to surface
  active goals, decisions taken (allow/deny/observe), budget
  consumption per axis, acceptance verdicts, escalation status,
  replay-policy verdicts (Phase 67.8 `agent.driver.replay`), and
  compact-policy triggers (Phase 67.9 `agent.driver.compact` —
  show `focus`, `token_pressure`, last compact turn). Settings
  panel must expose `compact_policy.{enabled,context_window,
  threshold,min_turns_between_compacts}`.
  Backend types ship in `nexo-driver-types` (67.0); the dashboard
  follows once 67.4 wires the loop and emits `agent.harness.*`
  events.
- [ ] Project-tracker tile (Phase 67.A) — render `PHASES.md` /
  `FOLLOWUPS.md` parsed state with status glyphs, current /
  next phase, last shipped, open follow-ups. Source of truth is
  the `nexo-project-tracker` crate; the tile subscribes to the
  watcher invalidation events.
- [ ] Multi-agent registry tile (Phase 67.B) — live list of every
  in-flight goal with status / turn N/M / wall / origin /
  dispatcher. Subscribe to `agent.registry.snapshot.*` for live
  refresh. Per-row drill-in shows snapshot detail + log buffer
  tail (`agent_logs_tail`).
- [ ] Dispatch surface (Phase 67.E + 67.G) — UI wrapping
  `program_phase`, `program_phase_chain`,
  `program_phase_parallel`, `dispatch_followup`. Form validates
  against the resolved `DispatchPolicy` (Phase 67.D.1) before
  submitting; preview lists exactly which sub-phases will be
  spawned and what hooks will be attached.
- [ ] Completion-hook designer (Phase 67.F) — attach
  `notify_origin` / `notify_channel` / `dispatch_phase` /
  `nats_publish` / `shell` hooks per goal. Shell hook entry is
  visually gated until the operator has flipped
  `PROGRAM_PHASE_ALLOW_SHELL_HOOKS` (Phase 67.A.5 capability).
- [ ] Capability matrix (Phase 67.D.1 / 67.H.3) — agent × binding
  grid showing `dispatch_capability` (none / read_only / full),
  `max_concurrent_per_dispatcher`, allowed / forbidden
  `phase_ids`. Hot-reload-aware: editing any cell republishes the
  YAML and triggers the runtime to rebuild the
  `ToolRegistryCache` (Phase 67.H.3) so the new surface lands
  without restart.
- [ ] Admin tools strip (Phase 67.G.4) — operator-only
  `set_concurrency_cap`, `flush_agent_queue`, `evict_completed`
  buttons gated by an `is_admin` flag at the session level.
- [ ] Subjects audit feed (Phase 67.H.2) — paged subscription to
  `agent.dispatch.{spawned,denied}`, `agent.tool.hook.
  {dispatched,failed}` so operators see every admission +
  refusal + hook outcome without grepping logs.
- [ ] Config hot-reload (Phase 18) — Phase A5 one-click reload
- [ ] Per-binding overrides — Phase A3 "Brain" tab needs the
  override matrix
- [ ] Poller framework (user's in-flight `crates/poller/`) — not
  yet wired; needs a "Pollers" section alongside Phases A2/A3 once
  the schema stabilises
- [ ] `runtime.yaml` (reload watcher settings) — small section in
  Phase A5
- [ ] `taskflow.yaml` (`tick_interval`, `timer_max_horizon`,
  `db_path`) — needs a TaskFlow operations panel: live `Waiting`
  flow list, manual resume/cancel, knob editor
- [ ] `transcripts.yaml` (`fts.enabled`, `fts.db_path`,
  `redaction.enabled`, `redaction.use_builtins`,
  `redaction.extra_patterns`) — needs a Transcripts panel:
  redaction toggles, pattern editor with live regex validation,
  index size + reindex trigger
- [ ] `agents.<id>.skill_overrides` + `requires.bin_versions` —
  Skills tab needs per-skill mode picker (strict/warn/disable)
  per agent, plus a constraint editor that probes the binary live
  and shows current version vs required
- [ ] 1Password inject command allowlist
  (`OP_INJECT_COMMAND_ALLOWLIST`) + secrets audit log viewer
  (`OP_AUDIT_LOG_PATH`) — Channels/Secrets panel needs an
  allowlist editor with live syntax check and a JSONL tail with
  filters by agent / session / action / ok-or-fail
- [ ] Pairing adapter registry — `pairing_inbound_challenged_total{channel,result}`
  counter ready to surface in admin metrics tab; per-channel
  delivery split (adapter vs broker fallback) needs a small panel
  alongside the existing pairing operator list
- [ ] Pairing health tile (Phase 26.y) — surface
  `pairing_requests_pending{channel}` (gauge), `pairing_approvals_total{channel,result}`,
  `pairing_codes_expired_total`, `pairing_bootstrap_tokens_issued_total{profile}`
  in the runtime dashboard. Approval ratio (`ok` vs `expired`) flags
  setup codes that age out before operators act; pending gauge needs
  a "refresh" button that calls `PairingStore::refresh_pending_gauge`
  to recover from in-memory drift after a daemon restart
- [ ] Capability toggles surface — pull
  `agent doctor capabilities --json`, render risk-coloured table,
  surface env var name and paste-ready export hint per row;
  warn on any `enabled` toggle so dangerous defaults are visible
- [ ] **MCP server over HTTP** (daemon) — `agent mcp-server`
  currently stdio-only; add HTTP transport (cookie auth, `/mcp/rpc`)
  to unlock remote IDE consumption. Tracked in detail under Phase A6
- [ ] **MCP completions / prompts / progress / cancellation /
  logging-receiver** — protocol gaps in `crates/mcp/`. See Phase A6
  "MCP — protocol gaps the daemon owes" for the full list
- [ ] **MCP server manager UI** — `mcp.yaml` CRUD from the admin
  with hot-reload. Phase A6
- [ ] **MCP live status** — per-spawned-server handshake / latency /
  last error. Phase A6 + needs new endpoint
- [ ] **`llm.context_optimization`** — four kill-switches
  (`prompt_cache`, `compaction`, `token_counter`, `workspace_cache`)
  per agent + global. Phase A3 "Brain" tab needs the override matrix
  (toggle per agent, inherits from global default). Phase A4 dashboard
  needs:
  - cache hit-ratio gauge per agent (`llm_cache_read_tokens_total` ÷
    sum of read+creation+input)
  - compaction history viewer (`compactions_v1` rows: when, how many
    head turns folded, summary text, token cost) with a "rebuild from
    audit" debug action
  - prompt-token drift histogram per agent
    (`llm_prompt_tokens_drift{agent,provider,model}`) so operators can
    spot when an approximate counter is misjudging the budget
  - workspace cache hit/miss/invalidation counters per workspace path
- [ ] **Pairing (Phase 26)** — `inbound_bindings[].pairing_policy` toggle
  (`auto_challenge: bool`) + a Pending Requests panel that lists
  `pairing_pending` rows with one-click approve and a soft-delete
  Revoke action against `pairing_allow_from`. Phase A3 "Brain" tab
  needs the per-binding toggle in the override matrix; a top-level
  Pairing tab needs the pending list, allow-from list with revoke,
  setup-code button (calls `nexo pair start --json` or its admin
  endpoint equivalent), and operator audit of `revoked_at`
  timestamps. Phase A4 dashboard needs `pairing_requests_pending`
  and `pairing_inbound_challenged_total` counters when the runtime
  starts emitting them (currently deferred — see FOLLOWUPS PR-1).
- [ ] **Web search (Phase 25)** — `agents.<id>.web_search` block
  (`enabled`, `provider`, `default_count`, `cache_ttl_secs`,
  `expand_default`) with per-binding override. Phase A3 "Brain" tab
  needs a provider picker (auto / brave / tavily / duckduckgo /
  perplexity), credential-presence indicator pulled from Phase 17
  credentials store, and the per-binding override row in the matrix.
  Phase A4 dashboard needs `web_search_calls_total` counter (planned)
  and breaker-state gauge per provider.
- [ ] **Link understanding (Phase 21)** — `agents.<id>.link_understanding`
  block (`enabled`, `max_links_per_turn`, `max_bytes`, `timeout_ms`,
  `cache_ttl_secs`, `deny_hosts`) with per-binding override.
  Phase A3 "Brain" tab needs a toggle + caps editor + denylist
  textarea, plus per-binding override row in the override matrix.
- [ ] **Proactive mode (Phase 77.20)** — `agents.<id>.proactive` and
  `inbound_bindings[].proactive` editor with:
  `enabled`, `tick_interval_secs`, `jitter_pct`, `max_idle_secs`,
  `initial_greeting`, `cache_aware_schedule`, `allow_short_intervals`,
  `daily_turn_budget`. Dashboard tile should chart
  `nexo_proactive_events_total{agent,event}` and highlight
  `sleep.interrupted` / budget suppression spikes.
- [ ] **Binding role switch (Phase 77.18)** — per-binding `role`
  selector (`coordinator`, `worker`, `proactive`) with runtime
  warnings when role/tool policy conflict. Worker role panel should
  preview the curated default tool surface and blocked tools
  (`Sleep`, `TeamCreate`, `TeamSendMessage`, `send_message`).
- [ ] **Loop skill visibility (Phase 77.12)** — include the bundled
  local skill `loop` in the per-agent Skills UX help text with its
  input contract (`prompt`, `max_iters`, `until_predicate`) and
  examples for `regex:` / `exit:` / `judge:` predicates.
- [ ] **Stuck skill visibility (Phase 77.13)** — include the bundled
  local skill `stuck` in the per-agent Skills UX help text with its
  debug contract (`failing_command`, `max_rounds`, `focus_pattern`)
  and examples for `cargo build` / `cargo test` failure triage.
- [ ] **Simplify skill visibility (Phase 77.14)** — include the bundled
  local skill `simplify` in the per-agent Skills UX help text with its
  simplification contract (`target`, `scope`, `max_passes`,
  `preserve_behavior`) and examples for dead-code cleanup /
  redundant-guard reduction / naming clarity.
- [ ] **Verify skill visibility (Phase 77.15)** — include the bundled
  local skill `verify` in the per-agent Skills UX help text with its
  verification contract (`acceptance_criterion`, `candidate_commands`,
  `max_rounds`, `judge_mode`) and examples for test/lint/type-check
  acceptance validation.
- [ ] **Per-agent setup dashboard** — model attach/detach, language pick, channel bind/unbind, skill toggle in one screen per agent (CLI shipped in `nexo setup` → `Configurar agente`; admin UI needs the same single-pane editor under each agent's detail view).
- [ ] **Pairing allowlist explorer (Phase 70.3)** — surface `PairingStore::list_allow` rows in the Pairing tab so operators can confirm seeded senders without dropping to `nexo pair list --all`. Columns: channel, account, sender, approved_via, approved_at, revoked_at. Filter by channel + toggle for "include revoked".
- [ ] **Pairing audit banner (Phase 70.6)** — when any binding has `pairing.auto_challenge: true` and zero rows in the allowlist for its `(channel, account)`, show a top-of-page banner with the `nexo pair seed` template — same audit `agent setup doctor` runs on the CLI.
- [ ] **Reload-flushes-gates badge (Phase 70.7)** — after a hot-reload, the dashboard event log should annotate the entry with the post-reload hooks that fired (e.g. `PairingGate.flush_cache`), so the operator sees why a freshly-seeded sender is now admitted without a restart.
- [ ] **Restart-survivor goals view (Phase 71.2)** — surface goals whose `status=LostOnRestart` in the agents tab, with a one-click "re-dispatch" button that calls `program_phase phase_id={…}` against the same origin. Matches the boot-time sweep so operators don't need to scroll the daemon log to know what got abandoned.
- [ ] **Shutdown drain log inspector (Phase 71.3)** — admin event timeline should highlight the `shutdown drain swept in-flight goals` line + report counters (`running_seen`, `hooks_fired`, `hook_dispatch_timeouts`) so operators can audit whether SIGTERM actually drained or hit the per-hook timeout.
- [ ] **Turn timeline (Phase 72)** — per-goal panel that streams `goal_turns` rows: one row per turn with outcome glyph, decision preview, summary, error, and a "show raw_json" expand. Filters: outcome (`needs_retry` only, `error` only). Admin should reuse the same SQLite table that `agent_turns_tail` reads, so the chat tool and the dashboard never disagree.
- [ ] **Email plugin (Phase 48)** — the email plugin lands a fresh
  per-instance surface that has no admin-UI yet. Phase A3 / A4
  needs:
  - **Account CRUD** under the channels matrix: instance, address,
    provider preset (Gmail / Outlook / Yahoo / iCloud / Custom),
    IMAP `host:port:tls`, SMTP `host:port:tls`, folders (inbox /
    sent / archive), `from_allowlist` / `from_denylist`. The CLI
    setup wizard is still deferred (see FOLLOWUPS) — the UI
    should land first or the wizard ship in parallel.
  - **Secrets editor** for `secrets/email/<inst>.toml` that hides
    the secret value, exposes the `kind` selector
    (`password` / `oauth2_static` / `oauth2_google`), and surfaces
    a "reuse google-auth account" picker when `provider = Gmail`.
    Enforce 0o600 on write.
  - **SPF / DKIM banner** showing the boot-warn matrix per account
    (`spf_missing`, `spf_misalignment`, `dkim_missing`,
    `dns_unavailable`) with the same operator-actionable hint
    text the daemon logs.
  - **`spf_dkim_warn` toggle** per plugin block + `loop_prevention`
    toggles (`auto_submitted` / `list_headers` / `self_from`)
    visible on the same page so an operator can mute warns
    without `vim`-ing the YAML.
  - **Bounce inbox** view backed by `email.bounce.<instance>`
    (when persisted bounce history lands; today the events fire
    but aren't stored).
  - **Outbound queue depth + DLQ inspector** — `health_map()`
    already exposes `outbound_queue_depth` / `outbound_dlq_depth`
    / `outbound_sent_total` / `outbound_failed_total` per account.
  - **`max_body_bytes` / `max_attachment_bytes` editor** with
    sensible defaults (32 KiB / 25 MiB).
  - **`EMAIL_INSECURE_TLS` capability badge** pulled from
    `setup::capabilities::INVENTORY` (registered 48.10) — warns
    red when armed in prod.
- [ ] **REPL stateful sandbox (Phase 79.12)** — per-agent `repl` config toggle
  (`enabled`, `allowed_runtimes`, `max_sessions`, `timeout_secs`,
  `max_output_bytes`) with per-binding override. Phase A3 "Brain" tab
  needs the toggle + runtime allowlist multi-select + session cap editor
  + per-binding override row in the matrix. Phase A4 dashboard needs
  live session list per agent (runtime, cwd, spawned_at, output_len)
  with one-click kill per session. Feature-gated behind `repl-tool` —
  UI should grey out when the daemon lacks the feature.
- [ ] **Assistant mode (Phase 80.15)** — per-binding `assistant_mode`
  block toggle on Phase A3 agent-config tab with `enabled` checkbox +
  `system_prompt_addendum` textarea (left blank shows the bundled
  default greyed-out as placeholder) + `initial_team` multi-select
  populated from the agent's `peers` directory. Boot-immutable flag
  surfaces a "restart required" banner when toggled. Phase A4 runtime
  dashboard shows an "Assistant mode active" badge per goal whose
  binding has the flag on. Backend already exposes
  `AgentConfig.assistant_mode: Option<AssistantConfig>` and
  `AgentContext.assistant: ResolvedAssistant`.
- [ ] **Auto-approve dial (Phase 80.17)** — per-binding `auto_approve`
  toggle in Phase A3 with workspace-path display + curated-tools
  preview (collapsible list showing every tool that auto-approves
  vs always-asks under the current binding). Phase A9 audit tab
  surfaces a per-turn "auto-approved tools" log line when 80.17.b.c
  caller-side metadata population ships. `setup doctor` warn for
  `assistant_mode + !auto_approve` misconfig (Phase 80.17.c) needs
  to render as a banner on the agent-config tab when both checkboxes
  disagree. Capability badge for `is_curated_auto_approve` decision
  table + decorator state.
- [ ] **Background sessions (Phase 80.10 + 80.16)** — Phase A4
  dashboard tab shows per-agent goal list filtered by `SessionKind`
  (chips: Interactive / Bg / Daemon / DaemonWorker) + status filter
  (Running / Sleeping / Done / etc.) + sort by `started_at` desc.
  Per-row drill-in mirrors `nexo agent attach <goal_id>` output
  (kind, status, phase, started, ended, last_progress, last_diff,
  turn N/M, last_event_at) with a "Live stream" placeholder
  (greyed) until 80.16.b NATS subscribe lands. New "Spawn BG goal"
  modal mirrors `nexo agent run --bg <prompt>`. New "Discover
  detached" pane mirrors `nexo agent discover` for the always-on
  view across reboots.
- [ ] **AWAY_SUMMARY digest (Phase 80.14)** — per-binding
  `away_summary` block on Phase A3 with `enabled` checkbox +
  `threshold_hours` numeric input (default 4, max 720) +
  `max_events` cap. Phase A9 audit tab shows the last digest text
  per (channel, sender_id) pair when 80.14.c `last_seen_at`
  pairing-store integration ships. Per-channel rendering preview
  (markdown vs whatsapp/telegram-flattened) when 80.14.d ships.
- [ ] **Multi-agent inbox (Phase 80.11 + 80.11.b)** — Phase A8
  delegation visualiser tab gains an "Inbox" pane per goal: live
  buffer count, oldest pending message timestamp, per-message
  preview (from / sent_at / body snippet / correlation_id?). Drain
  button (operator-side dev aid) clears the buffer. List/Send
  tools show in the Phase A6 tool-registry view with their current
  per-binding allow/deny status. Subject contract documented in
  the API reference panel: `agent.inbox.<goal_id>` JSON
  `InboxMessage` shape.
- [ ] **AutoDream cluster (Phase 80.1)** — Phase A7 memory inspector
  gains an "AutoDream" pane: status badge per binding (off / idle /
  fork-running / cooling-down), last consolidation timestamp,
  `dream_runs` audit table tail (mirrors `nexo agent dream tail`),
  per-run drill-in (mirrors `nexo agent dream status <run_id>`),
  manual "Force run" button mapping to `dream_now` LLM tool (gated
  behind `NEXO_DREAM_NOW_ENABLED` capability badge — green when
  armed, grey when not). Capabilities tab adds the `extension:
  "dream"` row from `INVENTORY`. `nexo agent dream kill <run_id>`
  per-row red button with "Are you sure?" confirm + memory-dir
  override input.
- [ ] **Webhook receiver dashboard (Phase 82.2)** — admin tab
  surfaces every configured webhook source: id, path,
  signature.algorithm, last-event timestamp, last-status
  counters (2xx / 4xx / 5xx) per source over the last 1 h /
  24 h. Inline "doctor" runs the static half of validate
  against the live config + env (secret presence). "Test"
  button signs a sample body locally and POSTs to the listener
  — useful smoke-test from the browser without leaving the
  admin UI. Source rows highlight red when `secret_env` is
  unset.
- [ ] **Event subscribers dashboard (Phase 82.4.b.b)** —
  per-binding event-rate counters, last-event timestamp,
  drop-oldest/drop-newest counters, mode badge
  (synthesize/tick/off), `nexo agent events test --binding
  <id>` invocation surface (CLI deferred to 82.4.d). Source
  rows highlight red when validation fails at boot
  (`tracing::error!` line tied to the row).
- [ ] (add lines as features land — see auto-memory rule)
