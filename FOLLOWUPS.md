# Follow-ups

This file tracks the **active technical backlog** in English.

Historical detailed notes that were previously written in Spanish are preserved at:
- `archive/spanish/FOLLOWUPS.es.txt`

## Rules

- After each `/forge ejecutar`, add any deferred work here.
- Keep each item with: what is missing, why it was deferred, and target phase.
- Move completed items to `Resolved` with a completion date.

## Current status

- Main roadmap phases are completed through Phase 19.
- Active work is now hardening, operational polish, and optional capability expansion.

## Open items

### Pollers (Phase 19 V2)

P-1. **`inventory!` macro registry for built-in pollers**
- Missing: compile-time auto-discovery so a new built-in lands by
  adding a single `pub mod` line, no `register_all` edit.
- Why deferred: pre-optimisation. The four current built-ins
  (gmail, rss, webhook_poll, google_calendar) plus extension-loaded
  pollers via the new `capabilities.pollers` capability are easy to
  maintain by hand; the explicit `register_all` is a useful audit
  point. Worth revisiting only when the list crosses ~20 entries.
- Target: when poller count grows.

P-2. **Multi-host runner orchestration**
- Missing: a coordinator that decides which host owns which job
  (the cross-process SQLite lease already prevents double-tick;
  what's missing is balanced placement and failover for tens of
  thousands of jobs spread across N daemons).
- Why deferred: speculative without a real multi-host deploy.
  Single-host workloads scale fine on the current model.
- Target: when a deployment actually needs >1 daemon.

P-3. **Push-based watchers (Gmail Push, generic inbound webhooks)**
- Missing: an HTTP server that accepts pushed events and adapts
  them to the same downstream `OutboundDelivery` plumbing the
  poller uses.
- Why deferred: opposite shape from polling — needs a public TLS
  surface (Cloudflare tunnel?) plus auth on inbound. Better as
  its own crate (Phase 20?), not an extension of the poller.
- Target: separate phase; keep notes here while it's only an idea.

### Hardening

H-1. **CircuitBreaker missing on Telegram + Google plugins**
- Missing: `nexo_resilience::CircuitBreaker` wrap around the
  Telegram bot HTTP calls (`crates/plugins/telegram/src/bot.rs`)
  and the Google OAuth + API calls
  (`crates/plugins/google/src/client.rs`). LLM clients, MCP HTTP
  client, and the browser CDP all use a breaker; these two are the
  outliers.
- Why deferred: needs a scoping decision (one breaker per
  channel-instance? one per agent? shared per plugin?). Wrapping
  every call site is ~100 lines per plugin and the pattern needs
  agreement before locking it in.
- Target: dedicated hardening session.

### Phase 21 — Link understanding

L-1. ~~**Telemetry counters for link fetches**~~  ✅ shipped
- `nexo_link_understanding_fetch_total{result}` (ok / blocked /
  timeout / non_html / too_big / error),
  `nexo_link_understanding_cache_total{hit}` (true / false), and a
  single-series `nexo_link_understanding_fetch_duration_ms` histogram
  emitted from `crates/core/src/link_understanding.rs::fetch`.
  Counters update on every fetch attempt; the histogram only fires
  when an HTTP request actually went out (cache hits and host-blocked
  URLs skip it to keep latency stats honest).

L-2. **`readability`-style extraction**
- Missing: the current extractor strips `<script>`/`<style>` and
  collapses whitespace, but it doesn't pick the main article block
  on noisy pages (sidebars, nav, footer all leak through). A real
  readability heuristic (or the `scraper` crate + DOM walk) would
  produce cleaner summaries.
- Why deferred: comment in `link_understanding.rs:17-20` flags this
  as a known compromise; naïve extraction is enough to validate the
  block-rendering pipeline end-to-end.
- Target: when an operator complains about noisy `# LINK CONTEXT`
  blocks polluting the prompt.

### Phase 25 — Web search

W-1. ~~**Telemetry counters not wired**~~  ✅ shipped
- `nexo_web_search_calls_total{provider,result}` (result ∈ ok /
  error / unavailable), `nexo_web_search_cache_total{provider,hit}`,
  `nexo_web_search_breaker_open_total{provider}`, and
  `nexo_web_search_latency_ms{provider}` histogram now emitted from
  `crates/web-search/src/telemetry.rs` and stitched into the host
  `/metrics` response by `nexo_core::telemetry::render_prometheus`.
  Latency is recorded only for attempts that actually issued an HTTP
  request — cache hits and breaker short-circuits skip it so
  percentiles reflect real provider work. The "unavailable" label
  distinguishes a breaker-open short-circuit from a real error so
  dashboards can alert without false positives during a self-healing
  cooldown.

W-2. **`web_fetch` built-in tool not shipped**
- Missing: spec'd a `web_fetch` companion tool that replaces the
  `fetch-url` extension. We deliberately scoped to `web_search` only
  in this round.
- Why deferred: Phase 21 `link_understanding` already auto-expands
  URLs the user shares. A generic `web_fetch` matters mostly for
  agentic workflows that compose `web_search → web_fetch → summarise`,
  and the `expand: true` flag on `web_search` covers most of that
  same loop today via the LinkExtractor reuse.
- Target: when a recipe surfaces the gap.

W-3. **Setup wizard entry not shipped**
- Missing: `agent setup web-search` to write API keys to credentials
  store. Currently operators set `BRAVE_SEARCH_API_KEY` /
  `TAVILY_API_KEY` in env directly.
- Why deferred: env vars work and the wizard is a pure UX win.
- Target: alongside admin-ui Phase A3 web-search panel.

W-4. **Decision: `nexo-resilience::CircuitBreaker` directly, not via `BreakerRegistry`**
- The `nexo-auth` registry is keyed on `Channel { Whatsapp,
  Telegram, Google }`. Web search isn't a channel; jamming it into
  that enum would force unrelated changes. We instead hold a
  per-provider `Arc<CircuitBreaker>` map inside the router. Worth
  unifying if more "non-channel external HTTP" surfaces land —
  bring it up next brainstorm.

W-5. **Cache `:memory:` SQLite quirk**
- The router cache pins `max_connections=1` when `path == ":memory:"`
  because SQLite's in-memory database is per-connection. File-backed
  paths use the normal pool size. Documented inline; not a defect.

### Phase 26 — Pairing protocol

PR-1. **Plugin gate hooks for WhatsApp + Telegram**
- Missing: actual `gate.should_admit(...)` call in
  `crates/plugins/whatsapp/src/inbound.rs` and
  `crates/plugins/telegram/src/bot.rs` before the broker publish.
- Why deferred: both plugins have parallel in-flight changes from
  other agents this cycle; jamming the hook in alongside risks
  conflict on rebases. The gate + store + CLI are landed and tested
  in isolation, so once the parallel work settles, wiring the hook
  is ~10 lines per plugin.
- Target: next plugin-side cycle. Pattern is documented in the spec
  (`PairingChannelAdapter` impl + gate call site).

PR-2. **Telemetry counters not wired**
- Missing: `pairing_requests_pending{channel}` (gauge),
  `pairing_approvals_total{channel,result}`,
  `pairing_codes_expired_total`,
  `pairing_bootstrap_tokens_issued_total`,
  `pairing_inbound_challenged_total{channel,result}`. Spec called
  for them; shipped without to keep the foundation commit small.
- Why deferred: same pattern as Phase 25 W-1 — admin-ui (Phase A4)
  is the eventual consumer.
- Target: Phase A4 dashboard work.

PR-3. **`tunnel.url` integration in URL resolver**
- Missing: `nexo pair start` currently honours only `--public-url`
  on the CLI. The spec'd priority chain includes `tunnel.url` as
  the second-highest source.
- Why deferred: the `nexo-tunnel` crate doesn't yet expose a
  read-only public-URL accessor. Adding one is a small refactor
  but slides into tunnel maintenance, not pairing.
- Target: when the tunnel crate ships an accessor.

PR-4. **Companion-tui not shipped**
- Missing: `apps/companion-tui/` reference companion that scans QR
  / accepts paste and opens the WS using the bootstrap token. Spec
  marks this as out of scope for the foundational phase.
- Why deferred: the protocol + CLI are sufficient for any third
  party to build a companion. A reference implementation is UX
  polish, not a blocker.
- Target: Phase 26.x or as a separate `companion-*` crate when
  demand arises.

PR-5. **`pair_approve` as scope-gated agent tool**
- Missing: a built-in tool that lets agents approve pending
  pairings from a trusted channel, scoped via
  `EffectiveBindingPolicy::allowed_tools`.
- Why deferred: opens prompt-injection vectors (an agent could be
  coerced into approving an attacker). Operator-driven approve via
  CLI / admin-ui is the safe default. Worth revisiting if a clear
  trust model emerges.
- Target: separate brainstorm.

PR-6. **`nexo-tunnel` URL accessor + `nexo-config::pairing.yaml` loader**
- Missing: spec'd a `config/pairing.yaml` with `storage.path`,
  `setup_code.secret_path`, `default_ttl_secs`, `public_url`,
  `ws_cleartext_allow`. Daemon currently hardcodes the paths
  (`<memory_dir>/pairing.db`, `~/.nexo/secret/pairing.key`) and
  reads `--public-url` only from the CLI flag.
- Why deferred: env vars / CLI flags cover the boot path; YAML
  adds a config surface with no current customer ask.
- Target: when an operator needs to override the path (for
  containerised deployments mounting `/var/lib/nexo/pairing/...`).

## Resolved (recent highlights)

- Streaming telemetry and streaming runtime wiring completed.
- Per-agent credentials hot-reload completed.
- Browser CDP reliability hardening completed.
- Shared extension resilience helpers extracted.
- Docs sync gate and mdBook English checks enabled.
- 2026-04-25 — SessionLogs tool registered in agent bootstrap and mcp-server (gated on non-empty `transcripts_dir`).
- 2026-04-25 — Skill dependency modes (`strict`/`warn`/`disable`) with per-agent `skill_overrides` + `requires.bin_versions` semver constraints (custom `command`/`regex` per bin). Probes are concurrent and process-cached. Banner inline for `warn` mode so the LLM sees missing deps.
- 2026-04-25 — 1Password `inject_template` tool (template-only with reveal gate, exec mode with `OP_INJECT_COMMAND_ALLOWLIST`, `dry_run` validation, stdout cap, redacted stdout/stderr) + append-only JSONL audit log (`OP_AUDIT_LOG_PATH`) covering `read_secret` and `inject_template` with `agent_id` / `session_id` context.
- 2026-04-25 — `agent doctor capabilities [--json]` CLI + `crates/setup/src/capabilities.rs` inventory: enumerates every write/reveal env toggle across bundled extensions (`OP_ALLOW_REVEAL`, `OP_INJECT_COMMAND_ALLOWLIST`, `CLOUDFLARE_*`, `DOCKER_API_*`, `PROXMOX_*`, `SSH_EXEC_*`) with state, risk, and revoke hints. Doc page `docs/src/ops/capabilities.md`.
- 2026-04-25 — TaskFlow runtime wiring: shared `FlowManager`, `WaitEngine` tick loop, `taskflow.resume` NATS bridge, and tool actions `wait`/`finish`/`fail` with guardrails (`timer_max_horizon`, non-empty topic+correlation).
- 2026-04-25 — Transcripts FTS5 index + redaction module: `transcripts.yaml` config, write-through index from `TranscriptWriter`, `session_logs search` uses FTS when present (substring fallback otherwise), opt-in regex redactor with 6 built-in patterns (Bearer JWT, sk-/sk-ant-, AWS access key, hex token, home path) and operator-defined `extra_patterns`.

## Maintenance note

If a future historical import includes non-English notes, keep them in `archive/spanish/*.txt` and update this Markdown tracker in English only.
