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

## Phase A6 — Extensions / skills / MCP   ⬜

- [ ] Extension discovery mirror: installed / disabled / invalid
- [ ] `agent ext install/uninstall/doctor --runtime` wrapped as UI
  actions
- [ ] MCP server manager: add external stdio / HTTP servers with
  header editor
- [ ] `agent mcp-server` on/off toggle with live link + copy
  snippet for Claude Desktop JSON config

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

- [ ] `credentials:` block (Phase 17) — agent-configuration tab
  Phase A3 covers this
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
- [ ] Capability toggles surface — pull
  `agent doctor capabilities --json`, render risk-coloured table,
  surface env var name and paste-ready export hint per row;
  warn on any `enabled` toggle so dangerous defaults are visible
- [ ] (add lines as features land — see auto-memory rule)
