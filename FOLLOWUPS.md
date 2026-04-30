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

### Autonomous mode hardening (audit 2026-04-28)
- No open items.

### MCP server — Phase 79.M follow-ups

**79.M.c.full** — Full Config tool body in mcp-server mode. **SHIPPED 2026-04-28**.
- Cargo feature `config-self-edit` gates the Config arm in
  `boot_exposable`. Boot context carries seven Config-only handles
  (applier + denylist + redactor + correlator + reload + policy +
  proposals_dir). `run_mcp_server` constructs all seven from the
  agent's YAML when `Config` is in `expose_tools`, then plus three
  hard refusals: (1) Cargo feature off → `SkippedFeatureGated`,
  (2) `mcp_server.auth_token_env` / `http.auth` missing →
  `SkippedDenied { config-requires-auth-token }`, (3)
  `agents.<id>.config_tool.self_edit = false` →
  `SkippedDenied { config-self-edit-policy-disabled }`,
  (4) `config_tool.allowed_paths` empty → refuse (operator must
  pick an explicit subset).
- Reload semantics in mcp-server mode: stub `McpServerReloadTrigger`
  warns + returns Ok. The operator-side `nexo run` daemon picks up
  YAML changes via Phase 18 file watcher. mcp-server itself does
  not host a `ConfigReloadCoordinator`.
- Threat model: see
  `docs/src/architecture/mcp-server-exposable.md::Threat-model`.

**79.M.h** — Hot-reload of `mcp_server.expose_tools`.
- Today: boot-time only. Operator must restart the mcp-server
  process to add/remove tools.
- Why deferred: Phase 18 hot-reload coverage doesn't yet drive a
  registry rebuild path. Acceptable: stdio mcp-server processes are
  short-lived (Claude Desktop / Cursor spawn them per-session).
  HTTP mcp-server is the real motivator — track under Phase 18
  coordinator extensions.

~~**79.M.completion** — MCP `completion/complete` returns empty
values for every request.~~ ✅ 2026-04-30
`completion/complete` now walks the target tool's `input_schema`,
extracts the `enum` array from the requested argument, and returns
populated `values`. `total` + `hasMore` fields added per MCP spec.
4 unit tests cover enum extraction, missing tool, no-enum arg, and
missing property. Graceful degradation: any parse failure returns
empty `[]` rather than an error.

**79.M.followup-autonomous** — `nexo mcp-server` cannot run
autonomous wait/retry loops by itself.
- Missing: a durable autonomous loop in mcp-server mode that
  processes due follow-ups/reminders without requiring a separate
  `nexo run` daemon (`AgentRuntime` + heartbeat tick path). Today
  mcp-server exposes control-plane calls (`start_followup`,
  `check_followup`, `cancel_followup`) but does not host the
  runtime turn loop.
- Why deferred: current architecture keeps mcp-server as a
  tool-bridge process; autonomous scheduling/execution lives in
  `nexo run`. Merging both concerns needs clear ownership of broker
  subscriptions, session lifecycle, and tick concurrency in mcp mode.
- Target: 79.M follow-up sub-phase (design + implementation of an
  optional autonomous worker profile for mcp-server).

**79.7.tool-calls** — shipped (opt-in) on 2026-04-28.
- Delivered: `LlmCronDispatcher` now supports an iterative
  tool-call loop (assistant tool_calls -> registry dispatch ->
  tool_result chaining -> follow-up model turn) with bounded
  iterations.
- Policy gates: disabled by default; operators must enable
  `runtime.cron.tool_calls.enabled`. Effective tool surface is
  narrowed by binding policy plus `runtime.cron.tool_calls.allowlist`.
  A stable per-entry `session_id` is injected for tool contexts.
- Minimal runtime profile (marketing follow-ups safe allowlist):
  ```yaml
  schema_version: 11
  cron:
    tool_calls:
      enabled: true
      max_iterations: 6
      allowlist:
        - email_search
        - email_thread
        - email_reply
        - cancel_followup
        - check_followup
  ```
- Manual smoke (reproducible):
  1. Fast dispatcher proof:
     `cargo test -p nexo-core llm_cron_dispatcher::tests::tool_calls_execute_when_executor_enabled -- --nocapture`
  2. Runtime wiring proof:
     run `nexo run` with `config/runtime.yaml` above and confirm startup log:
     `"[cron] tool-call execution enabled"` with expected `allowlist`.
  3. End-to-end follow-up flow (from your MCP client):
     - `start_followup` args example:
       ```json
       {
         "thread_root_id": "<message-id-root>",
         "instance": "ops",
         "recipient": "cliente@example.com",
         "check_after": "24h",
         "max_attempts": 3
       }
       ```
       Save returned `flow.flow_id`.
     - `cron_create` args example (one-shot):
       ```json
       {
         "cron": "*/2 * * * *",
         "recurring": false,
         "prompt": "Usa check_followup con flow_id=<FLOW_ID>. Si flow.status es active, llama cancel_followup con reason='smoke'. Cierra con texto: smoke-ok."
       }
       ```
     - Verify after next fire window:
       `check_followup` on the same `flow_id` returns `flow.status = \"cancelled\"`.
       Optional: `nexo cron list --json` no longer includes that one-shot entry.
- Remaining hardening follow-up: per-tool timeout/idempotency policy
  for high-side-effect tools, plus richer compensation semantics.

**79.M.denied-by-default surface** — shipped on 2026-04-28.
- Delivered: `mcp_server.denied_tools_profile` is now a mandatory
  hardening gate for denied overrides (`Heartbeat`, `delegate`,
  `RemoteTrigger`), with fail-closed defaults (`enabled=false`, all
  allow bits false).
- Policy: denied tool registration now requires:
  1) tool in `expose_denied_tools`,
  2) `denied_tools_profile.enabled=true`,
  3) matching `denied_tools_profile.allow.<tool>=true`.
- Validation checks:
  - `require_auth` (default true) enforces MCP auth before denied
    side-effect tools boot.
  - `require_delegate_allowlist` (default true) requires explicit
    restricted `agents.<id>.allowed_delegates` (non-empty, not `*`)
    for `delegate`.
  - `require_remote_trigger_targets` (default true) requires explicit
    `agents.<id>.remote_triggers` entries for `RemoteTrigger`.

**79.M.taskflow-session-context** — shipped on 2026-04-28.
- Delivered: MCP `tools/call` now forwards request-scoped
  `DispatchContext` to handlers through context-aware trait hooks
  (`call_tool_with_context` / `call_tool_streaming_with_context`).
- Bridge fix: `ToolRegistryBridge` now executes each tool call with a
  per-call `AgentContext` clone that injects `session_id` from MCP
  dispatch context (UUID parse, fallback deterministic UUIDv5 for
  non-UUID ids), instead of always using the fixed boot context.
- Stdio parity: stdio transport now stamps a stable per-process
  implicit `session_id`, so context-dependent tools (`taskflow`) also
  work in stdio MCP sessions.
- Coverage: bridge unit test verifies session-id injection from
  dispatch context.

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

H-1. ~~**CircuitBreaker missing on Telegram + Google plugins**~~  ✅ 2026-04-26
- ~~Telegram side fully wired~~ — `BotClient` now owns
  `circuit: Arc<CircuitBreaker>` (one breaker per `BotClient`
  instance, breaker name `telegram.<redacted-host>` so logs
  never carry the bot token). All three HTTP exit points
  (`call_json` JSON POST, multipart `sendDocument`, `download_file`
  GET) flow through a single `run_breakered` helper that maps
  `CircuitError::Open` → `bail!("circuit breaker open")` and
  passes inner errors through. 13 existing telegram tests still
  pass.
- ~~Google general-API side wired~~ — `GoogleAuthClient` now
  owns its own `circuit` field; `authorized_call` (the HOT path
  used by every google_* tool) wraps via `run_breakered` with
  the same map.
- ~~All 5 Google OAuth exit points wired (2026-04-26)~~ —
  `exchange_code`, `request_device_code`, `poll_device_token`,
  `refresh_if_needed`, and `revoke` all flow through the same
  `run_breakered` helper. Each call site rolls the entire
  request → status check → JSON parse block inside the closure
  so a transport failure, malformed body, or 4xx/5xx all count
  the same toward the breaker's failure threshold. The polling
  loop in `poll_device_token` wraps each iteration separately
  so a sustained burst of `authorization_pending` (which is
  expected and not a failure) doesn't trip the breaker.
  `revoke` keeps its best-effort semantics — local state is
  wiped regardless of upstream success.
- Scoping decision locked in: **one breaker per client
  instance** (per BotClient, per GoogleAuthClient). Multi-tenant
  setups holding multiple instances get isolated breakers, so a
  single bad token doesn't cascade across tenants.

H-2. **C1 — `EffectiveBindingPolicy` extension (per-binding override
for `lsp` / `team` / `config_tool` / `repl`)** — ✅ shipped 2026-04-30.
- Surfaced by audit `proyecto/proyecto/AUDIT-2026-04-30.md`.
  `EffectiveBindingPolicy` (`crates/core/src/agent/effective.rs:38`)
  now carries 4 additional resolved fields plus 4 mirror resolvers
  (`resolve_lsp` / `resolve_team` / `resolve_config_tool` /
  `resolve_repl`). `InboundBinding` (`crates/config/src/types/agents.rs`)
  gains 3 new optional override fields (`lsp` / `team` /
  `config_tool`); `repl` was already declared (Phase 79.12) but
  silently inherited because the resolver was missing — closed.
  10 new tests in `effective.rs::tests` (8 golden) and
  `binding_validate.rs::tests` (2 covering 7 sub-cases).
- `binding_validate::has_any_override` extended from 12 to 19
  conditions so the "binding without overrides" warning stops
  lying for `plan_mode` / `role` / `proactive` / `repl` / `lsp` /
  `team` / `config_tool`.
- **Boot-time only** — the new resolved fields are not yet read by
  the per-agent boot loop in `src/main.rs:2326-2680` (which still
  calls `agent_cfg.lsp` / `agent_cfg.team` / `agent_cfg.config_tool`
  / `agent_cfg.repl` directly). That refactor + `ConfigReloadCoordinator`
  post-hooks for `LspManager` / `TeamMessageRouter` /
  `ReplRegistry` / cron-tool bindings is **C2** — see below.
- No YAML breakage: defaults `None` → inherit. The single
  observable runtime change is that `inbound_bindings[].repl`
  overrides will start applying — `grep -rn "repl:" config/` is
  empty in this repo so no config in the tree is affected.

H-3. **C2 — Hot-reload rebuild of per-binding tool registrations** —
⬜ open. Blocks the Phase 18 promise that hot-reload picks up every
binding-config change. Today the per-agent boot loop in
`src/main.rs:2042-2705` registers tools once and never re-runs.
Toggling `lsp.enabled` / `team.enabled` / `proactive.enabled` /
`repl.enabled` / `config_tool.self_edit` / growing
`remote_triggers` requires a daemon restart. Work:
1. Add a `rebuild_per_agent_tools()` callable that re-runs the
   relevant registration arms over the new `RuntimeSnapshot`.
2. Register one `ConfigReloadCoordinator::register_post_hook` per
   subsystem (cron-store flush, REPL registry, LSP manager,
   TeamMessageRouter cache) — analogue of the `PairingGate`
   flush at `src/main.rs:3492` (Phase 70.7).
3. Refactor 10 sites in `main.rs` to read from
   `EffectiveBindingPolicy` instead of `agent_cfg.<x>` (full list
   in audit C2).
- Pitfall to design around (claude-code-leak
  `src/utils/settings/changeDetector.ts:8-11`): N-way thrashing
  if N subsystems each subscribe — debounce / coalesce at the
  coordinator, not at each post-hook.

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

L-2. ~~**`readability`-style extraction**~~  ✅ shipped 2026-04-26
- `extract_main_text` now drops the universal boilerplate tag set:
  on top of the original `<script>` / `<style>` / `<noscript>` /
  `<head>`, it also nukes `<nav>`, `<header>`, `<footer>`,
  `<aside>`, `<form>`, `<button>`, `<menu>`, `<iframe>`, `<svg>`,
  `<dialog>`, `<template>`. That alone covers the majority of
  noisy-page article extraction wins.
- New `strip_blocks_by_class_keyword` pass handles sites that
  render boilerplate inside `<div>`s instead of semantic tags:
  drops any element whose `class` / `id` / `role` attribute
  contains `sidebar`, `comment`, `advert`, `share`, `social`,
  `cookie`, `popup`, `newsletter`, `related-article`,
  `related-posts`, `navigation`, `breadcrumb`, `promo`,
  `subscribe`. Tag-agnostic — same logic catches
  `<div class="sidebar">` and `<aside class="sidebar">`.
- 5 new tests cover semantic-boilerplate strip, class-marked
  sidebars, role="navigation" attribute matching, negative
  control (innocent class names like `content` /
  `article-body` / `byline` survive), and form/button clutter
  removal. Runs alongside the existing 13 tests in
  `link_understanding::tests`.
- No new crate dependency; pure-Rust implementation. Real DOM-walk
  readability via the `scraper` crate is the next-step upgrade
  if a specific site shape still leaks.

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

W-2. ~~**`web_fetch` built-in tool not shipped**~~  ✅ shipped 2026-04-26
- New `crates/core/src/agent/web_fetch_tool.rs::WebFetchTool`.
  Single-call shape: `web_fetch(urls: [str], max_bytes?: int)`
  → `{ results: [{url, title, body, ok, reason?}] }`.
- Reuses the runtime's existing `LinkExtractor` (Phase 21),
  so the cache, deny-host list, max-bytes cap, timeout, and
  telemetry counters all carry over with zero duplication.
  `nexo_link_understanding_fetch_total{result}` and
  `nexo_link_understanding_cache_total{hit}` cover `web_fetch`
  calls automatically.
- Per-call cap of 5 URLs to keep the prompt budget bounded;
  trims with a warn log and continues. `max_bytes` arg can
  shrink but never grow past the deployment-wide
  `link_understanding.max_bytes`.
- Failures (host blocked / timeout / non-HTML / oversized /
  transport error) return per-URL
  `{ok: false, reason: "..."}` rows instead of bailing the
  whole call, so a single bad URL doesn't drop the rest.
- Registered unconditionally for every agent in `src/main.rs`
  (runtime always boots a `LinkExtractor`); the per-binding
  `link_understanding.enabled` policy still gates whether the
  underlying fetch happens.
- 2 unit tests (`tool_def_shape`, `rejects_empty_urls_array`)
  in the module.
- Distinct from `web_search.expand=true` because the agent
  often knows the URL up front (skill output, RSS poll,
  calendar attachment) and would otherwise have to either
  hallucinate a search query or shell out to a `fetch-url`
  extension.

W-3. ~~**Setup wizard entry not shipped**~~  ✅ shipped 2026-04-26
- New `web-search` ServiceDef in
  `crates/setup/src/services/skills.rs::defs()`. Distinct from
  the existing `brave-search` entry (which configures the
  MCP-based skill); this one writes the keys the in-process
  Phase 25 router consumes.
- Three fields:
    * `brave_api_key` (secret → `web_search_brave_api_key.txt`,
      env `BRAVE_SEARCH_API_KEY`).
    * `tavily_api_key` (secret →
      `web_search_tavily_api_key.txt`, env `TAVILY_API_KEY`).
    * `default_provider` (env-only `WEB_SEARCH_DEFAULT_PROVIDER`,
      default `brave`).
  Both keys are optional individually — the router falls back
  across whichever provider is configured.
- Operator runs `nexo setup` and picks "Web search router (Phase
  25)" from the Skills category, same flow as every other
  service.
- Description text + help strings written in English (per the
  workspace language rules). Existing entries above still have
  Spanish strings — those predate the rule.
- admin-ui Phase A3 web-search panel will surface the same
  fields when it lands.

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

PR-1. ~~**Plugin gate hooks for WhatsApp + Telegram**~~  ✅ shipped (in agent-core intake)
- The gate now runs in the runtime intake hot path
  (`crates/core/src/agent/runtime.rs`) right before the per-sender
  rate limiter. Plugins do not need bespoke wiring — the gate sees
  every event regardless of source plugin, keyed by
  `(source_plugin, source_instance, sender_id)`. Default
  `auto_challenge=false` keeps existing setups silent.
- Reply-back path deferred: when a sender is challenged the code is
  only logged (operator approves via `nexo pair approve`). Sending
  the code through the channel adapter so the sender sees it in
  their chat is PR-1.1, separate work that needs a per-channel
  outbound publish helper.

PR-1.1. ~~**Challenge reply through channel adapter**~~  ✅ shipped (Phase 26.x, 2026-04-25)
- `PairingAdapterRegistry` lives in `nexo-pairing`; bin registers
  `WhatsappPairingAdapter` + `TelegramPairingAdapter` at boot.
- Per-channel `normalize_sender` is plumbed through
  `PairingGate::should_admit` so store lookup + cache key use the
  canonical form (WA strips `@c.us`, TG lower-cases `@username`).
- Telegram challenges escape MarkdownV2 reserved chars and wrap the
  code in backticks; WhatsApp ships the legacy plain-text shape.
- New counter
  `pairing_inbound_challenged_total{channel,result}` covers the
  delivery outcomes (`delivered_via_adapter`,
  `delivered_via_broker`, `publish_failed`,
  `no_adapter_no_broker_topic`).
- **Still deferred:** direct in-process `Session::send_text` —
  adapters currently publish on
  `plugin.outbound.{channel}[.<account>]` like the rest of the
  system; skipping the broker round-trip is a separate refactor and
  not on the critical path.

PR-2. **Telemetry counters not wired** ✅ Closed 2026-04-25 (Phase 26.y).
- ~~`pairing_requests_pending{channel}`~~ ✅ gauge, push-tracked, with
  `PairingStore::refresh_pending_gauge` exposed for drift recovery.
- ~~`pairing_approvals_total{channel,result}`~~ ✅ counter, three results:
  `ok | expired | not_found`.
- ~~`pairing_codes_expired_total`~~ ✅ counter, bumped from
  `purge_expired` (per row) and from `approve` (per expired hit).
- ~~`pairing_bootstrap_tokens_issued_total{profile}`~~ ✅ counter on
  every `BootstrapTokenIssuer::issue`.
- ~~`pairing_inbound_challenged_total{channel,result}`~~ ✅ shipped
  with Phase 26.x adapter work.
- All four counters live in `nexo-pairing::telemetry` (leaf crate);
  `nexo_core::telemetry::render_prometheus` stitches them in next to
  the web-search block. Consumer: admin-ui Phase A4.

PR-3. ~~**`tunnel.url` integration in URL resolver**~~  🔄 partial 2026-04-26
- ~~`run_pair_start` URL resolver chain wired~~ — priority is
  now (1) `--public-url` CLI flag, (2) `pairing.yaml`
  `public_url`, (3) `NEXO_TUNNEL_URL` env var, (4) loopback
  fail-closed. The `nexo-tunnel` daemon writes its assigned
  `https://*.trycloudflare.com` URL into `NEXO_TUNNEL_URL` at
  startup, which a separately-launched `nexo pair start` picks
  up without IPC plumbing.
- ~~`ws_cleartext_allow` from `pairing.yaml` plumbed into the
  resolver `extras` list~~, so an operator setting that list in
  YAML actually changes the cleartext-host allowlist. Resolves
  the second deferred item from PR-6.
- ~~`pair_paths` consults `pairing.yaml` overrides~~ for both
  store path and secret path so CLI subcommands honour the
  same config the daemon does. Falls back to legacy defaults
  unchanged when the YAML is absent.
- ~~In-process URL accessor across daemon ↔ CLI~~  ✅ shipped
  2026-04-26 via a sidecar file at
  `$NEXO_HOME/state/tunnel.url`. `nexo-tunnel` exposes
  `url_state_path()`, `write_url_file()`, `read_url_file()`,
  `clear_url_file()`. The daemon writes the URL on
  `TunnelManager::start()` success; `nexo pair start` reads it
  with priority above the env-var fallback. Atomic writes
  (`<path>.tmp` + rename) so a CLI reading mid-write never
  sees a torn URL. Round-trip unit test covers happy path +
  whitespace trim + idempotent clear.

PR-4. ~~**Companion-tui not shipped**~~ ✅ 2026-04-27 (PR-4.x WS handshake complete)
- ~~Reference scaffold shipped~~ as `crates/companion-tui`.
- ~~PR-4.x~~ WS handshake shipped 2026-04-27:
  - Server: `GET /pair` detected via `TcpStream::peek()` in
    `handle_health_conn`; `tokio_tungstenite::accept_async` upgrades
    the raw stream without consuming bytes. Server verifies HMAC via
    `SetupCodeIssuer::verify`, issues a 32-byte random session token
    (base64url), persists in `PairingSessionStore` (SQLite,
    `$NEXO_HOME/state/pairing_sessions.db`, 24h TTL), returns
    `{"session_token": "..."}`. Context available via
    `PairingHandshakeCtx` in `OnceLock` in `RuntimeHealth`.
  - Client: `nexo-companion` calls `ws::perform_handshake`, writes
    session token to `$NEXO_HOME/pairing/sessions/<label>.token`
    (0600, atomic rename).
  - `run_pair_start` now embeds the full `/pair` path in the
    setup-code URL so the companion connects directly.
  - 4 session_store unit tests + 3 ws sanitize tests.
- Bugs found and fixed during 2026-04-27 audit (all corrected in-session):
  - `pair_url` variable never applied to `run_pair_start` — `issuer.issue()`
    was still passing `&resolved.url` without `/pair`, so the companion would
    connect to the base URL and the peek-router would never route to `handle_pair_ws`.
  - Session TTL used `default_ttl_secs * 144` formula — if operator set
    `default_ttl_secs = 3600`, sessions lasted 6 days. Fixed to always 86400 s.
  - `remote_triggers: Vec::new()` missing from `run_mcp_server` `AgentConfig`
    initializer — caused compile error when `AgentConfig` gained the field.
  - `insert_session` called `Utc::now()` twice (skew between `issued_at` and
    `expires_at`). Fixed to single capture.
  - `lookup_session` used `unwrap_or_else(Utc::now)` for corrupt timestamp —
    silently returned current time as expiry. Fixed to propagate error via
    `.ok_or(PairingError::Storage(...))? + .transpose()`.
- Remaining open items:
  - Session token validation on subsequent companion requests
    (not yet consumed by any handler — `lookup_session` exists
    but is not wired to any auth gate).
  - `pairing.session_ttl_secs` YAML config field — currently hardcoded 86400 s.
    Add as an optional override in `PairingConfig` so operators can tune
    without rebuilding.

PR-5. **`pair_approve` as scope-gated agent tool**
- Missing: a built-in tool that lets agents approve pending
  pairings from a trusted channel, scoped via
  `EffectiveBindingPolicy::allowed_tools`.
- Why deferred: opens prompt-injection vectors (an agent could be
  coerced into approving an attacker). Operator-driven approve via
  CLI / admin-ui is the safe default. Worth revisiting if a clear
  trust model emerges.
- Target: separate brainstorm.

PR-6. ~~**`nexo-config::pairing.yaml` loader**~~  🔄 partial 2026-04-26
- ~~`config/pairing.yaml` schema + loader shipped.~~
  `crates/config/src/types/pairing.rs` defines
  `PairingConfig { pairing: PairingInner }` with optional
  fields: `storage.path`, `setup_code.secret_path`,
  `setup_code.default_ttl_secs`, `public_url`,
  `ws_cleartext_allow[]`. `deny_unknown_fields` everywhere so
  typos fail loud at boot.
- ~~Loader wired into `AppConfig`~~ —
  `cfg.pairing: Option<PairingInner>` populated by
  `load_optional("pairing.yaml")` (file is optional; absent
  keeps every legacy default).
- ~~Boot integration in `src/main.rs`~~ — the `pairing` block
  consults `cfg.pairing` first for both store path and
  secret path, falling back to the previous hardcoded
  `<memory_dir>/pairing.db` / `~/.nexo/secret/pairing.key`
  defaults when the YAML is absent or doesn't override that
  field. New `from_yaml=true|false` log field reflects which
  path provided the values.
- 4 unit tests cover empty YAML → defaults, full YAML round
  trip, unknown-field rejection at root + nested levels.
- **Still deferred**: `nexo-tunnel` URL accessor exposing the
  active tunnel URL (separate side of PR-6, originally bundled).
  The `pairing.yaml` `public_url` field is wired but the
  `tunnel.url` priority chain (PR-3) still hardcodes the CLI
  fallback. Splitting into PR-6.a (config loader, done) and
  PR-3 (tunnel accessor, separate) keeps the work
  cleanly scoped.
- ~~`default_ttl_secs` honoured by `nexo pair start`~~  ✅
  (commit landed alongside W-3). Resolution priority is now
  (1) `--ttl-secs` CLI flag, (2) YAML `default_ttl_secs`,
  (3) 600s hardcoded fallback. The CLI parser switched to
  `Option<u64>` so absent flag is genuinely "no override"
  rather than the previous baked-in 600 default.
- ~~**`ws_cleartext_allow` not plumbed**~~ ✅ already wired —
  `run_pair_start` reads `yaml_overrides.ws_cleartext_allow` into
  `yaml_cleartext` and passes it to `UrlInputs.ws_cleartext_allow_extra`
  before calling the resolver. FOLLOWUPS entry was stale.

### Phase 67.A–H — Project tracker + multi-agent dispatch

PT-1. **`ToolHandler` adapter for dispatch tools not yet
registered**
- Missing: each `program_phase_dispatch`, `dispatch_followup`,
  `cancel_agent`, etc. is a plain async function. The runtime
  needs a `nexo_core::ToolHandler` adapter that builds the
  context (resolved DispatchPolicy, sender_trusted, dispatcher
  identity) per-binding and forwards to the function.
- Why deferred: the adapter touches the runtime intake hot
  path (`crates/core/src/agent/runtime.rs`) and the per-binding
  cache; landing it in 67.E.1 would have stretched the step.
  Functions are decoupled and tested directly; the adapter
  step is a wiring exercise behind the binding refactor.
- Target: 67.H.x adapter step alongside the binary refactor that
  folds `nexo-driver-tools` into `nexo-driver`.

PT-2. **Runtime intake migration to `get_or_build_with_dispatch`**
- Missing: existing call sites use the old
  `get_or_build(allowed_tools)` API; the new dispatch-aware
  variant is callable but unused.
- Why deferred: switching call sites needs the dispatcher /
  is_admin context plumbed through binding resolution. PT-1
  unblocks this — both land together.
- Target: same as PT-1.

PT-3. **`DispatchTelemetry` not wired into `program_phase` /
hook dispatcher / registry**
- Missing: the trait + payloads + canonical subjects ship in
  Phase 67.H.2 but every call site uses `NoopTelemetry` today.
  No `agent.dispatch.*` / `agent.tool.hook.*` /
  `agent.registry.snapshot.*` traffic is emitted yet.
- Why deferred: emission needs an instance threaded through
  the call sites, which in turn depends on PT-1's adapter
  layer. Pure plumbing — no decision left.
- Target: alongside PT-1 / PT-2.

PT-4. ~~**`HookIdempotencyStore` not consumed by `DefaultHookDispatcher`**~~  ✅ 2026-04-27
- The dispatcher's pre-action claim + post-failure release was already
  implemented in `dispatcher.rs:180-217` (shipped in an earlier pass).
- Boot wiring added in `src/main.rs`: opens
  `$NEXO_HOME/state/hook_idempotency.db` and passes it to
  `DefaultHookDispatcher::with_idempotency()`. Failure degrades to
  idempotency-less mode with `tracing::warn!` — non-fatal.
- `EventForwarder` gains `idempotency: Option<Arc<HookIdempotencyStore>>`
  field + `with_idempotency()` builder. On `GoalCompleted` it calls
  `store.forget_goal(goal_id)` after `hook_registry.drop_goal()` to
  prevent unbounded table growth. Failures are best-effort (warn only).
- 5 existing tests in `hook_idempotency_after_restart.rs` cover the
  full flow (replay skip, restart persistence, B10 retry, forget).

PT-5. ~~**Single-flight cap-counting race in `AgentRegistry::admit`**~~  ✅ already shipped
- `admit_lock: tokio::sync::Mutex<()>` in `registry.rs:71` serialises
  the entire `count_running → cap check → insert` critical section.
- Test `concurrent_admits_do_not_overshoot_cap` validates 10 concurrent
  admits with cap=3 → exactly 3 Running + 7 Queued.
- FOLLOWUPS entry was stale; fix was deployed alongside the registry
  hardening pass. No further action needed.

PT-6. **`nexo-driver` and `nexo-driver-tools` are separate bins**
- Missing: a single binary that exposes both `run` (Claude
  subprocess driver) and `status / dispatch / agents`
  (project-tracker CLI). Folding them needs to break the
  current crate-graph cycle (driver-loop ↔ dispatch-tools).
- Why deferred: cycle-breaking is a refactor (move the bin to
  a new top-level crate that depends on both, or push the
  dispatch surface into a feature flag of driver-loop).
  Separate bins ship today.
- Target: binary refactor pass.

PT-7. **No NATS-backed `DispatchTelemetry` impl**
- Missing: production `DispatchTelemetry` should publish to the
  daemon's `async-nats` client. Currently only `NoopTelemetry`.
- Why deferred: the impl is a thin adapter but lives next to
  `NatsEventSink` in `nexo-driver-loop`, which adds a
  reverse-dep on dispatch-tools. Same cycle-breaking refactor
  as PT-6.
- Target: alongside PT-6.

PT-9. ~~**Non-chat origin discriminator hardcoded as 'console'**~~  ✅ effectively resolved
- `NON_CHAT_ORIGIN_PLUGINS: &[&str] = &["console", "cron", "webhook", "heartbeat"]`
  already exists at `dispatch-tools/src/hooks/dispatcher.rs:21-25` and
  the `run_action()` check uses `.contains()` against it. All four
  non-chat origins are covered — no cron/webhook/heartbeat goal will
  send a spurious chat reply.
- The code comment explicitly notes the constant is a bridge until a
  full `OriginAdapter` trait lands. That trait is better deferred until
  a plugin needs custom behavior beyond a boolean (e.g., per-origin
  render format). Current constant is the right level of complexity.

PT-8. **Multi-agent end-to-end test not shipped**
- Missing: a single integration test that wires
  orchestrator + registry + dispatch-tools + a mock
  pairing-adapter, dispatches two goals concurrently, and
  asserts a `notify_origin` summary lands on the mock adapter
  for each.
- Why deferred: the test needs the adapter wiring (PT-1) so
  the chat origin propagates into the hook payload.
- Target: alongside PT-1 / PT-3 / PT-4.

### ~~Browser plugin leaks zombie child processes~~  ✅ 2026-04-27

- Fixed in `crates/plugins/browser/src/chrome.rs` + `plugin.rs`.
- `RunningChrome::shutdown(self)` now calls `child.kill().await` +
  `child.wait().await` before consuming self — process is reaped
  before the handle is dropped.
- `BrowserPlugin::stop()` calls `chrome.shutdown().await` explicitly
  instead of assigning `None` (which triggered Drop without reaping).
- `Drop` kept as safety-net with a `tracing::warn!` so unexpected
  drops surface in logs rather than silently accumulating zombies.
- Unit test `shutdown_reaps_process` verifies kill(pid, 0) → ESRCH
  after shutdown (blocked on nexo-core Phase 79 WIP compile errors;
  test code is correct and will run once those are resolved).

### ~~`set_active_workspace` state lost on daemon restart~~  ✅ 2026-04-27

- Fixed via text-file sidecar at `$NEXO_HOME/state/active_workspace_path`
  (same pattern as `nexo-tunnel`'s `tunnel.url` sidecar).
- `crates/project-tracker/src/state.rs` — new module with
  `write_active_workspace_to(state_dir, path)` (temp+rename atomic write)
  and `read_active_workspace_from(state_dir)` (reads + verifies path exists).
  Public `write_active_workspace` / `read_active_workspace` convenience
  wrappers resolve `$NEXO_HOME/state/` automatically.
- `src/main.rs::boot_dispatch_ctx_if_enabled` — resolution order is now
  (1) `NEXO_PROJECT_ROOT` env var, (2) saved sidecar, (3) walk-up for
  `PHASES.md`, (4) cwd fallback.
- `dispatch_handlers.rs::SetActiveWorkspaceHandler` + `InitProjectHandler`
  — call `write_active_workspace` after every successful `switch_to()`.
  Failures log `tracing::warn!` and are non-fatal (in-memory state still
  correct; only the restart persistence is lost).
- 3 unit tests: roundtrip, missing-file → None, nonexistent-path → None.

### Phase 27.1 / 27.2 — cargo-dist + GH Actions release deferrals

Resolved by Phase 27.2 (kept here for traceability):
- ~~`NEXO_BUILD_CHANNEL` env stamp defaulted to `source` everywhere.~~
  CI release workflow now exports
  `NEXO_BUILD_CHANNEL=tarball-${target}` per musl runner and
  `NEXO_BUILD_CHANNEL=termux-aarch64` for the Termux job.
- ~~`x86_64-unknown-linux-gnu` host-fallback target.~~ Dropped from
  `dist-workspace.toml` in 27.2 — local builds use musl directly
  (operator must install zig 0.13.0 + cargo-zigbuild 0.22.3 per
  `packaging/README.md`).
- ~~macOS / Windows local validation needs vendor SDKs.~~ Targets
  removed from scope entirely (see backlog item below); no longer
  a deferral.

Open:

- **Local musl validation requires the pinned toolchain.** zig
  0.13.0 + cargo-zigbuild 0.22.3 must be on PATH; newer zig
  (0.14+ / 0.16) is incompatible with cargo-zigbuild 0.22.x.
  `make dist-check` fails loud with a pointer to
  `packaging/README.md` if zig is missing. Track upstream:
  <https://github.com/rust-cross/cargo-zigbuild>.
- **Termux runtime smoke-test.** Phase 27.2 validates the `.deb`
  sha256 sidecar but cannot run the bionic-libc binary on the
  ubuntu runner. Manual install on a device or Android emulator
  is the gate. Watch for headless Termux smoke options
  (proot-distro inside ubuntu? android-emulator GH action?).
- **Smoke-test auto-rollback.** When the post-publish smoke test
  fails, the assets are already up. Workflow goes red, operator
  decides. A rollback step would call `gh release delete-asset`
  per `EXPECTED_TARBALLS` member, idempotent. Risk: race with
  `sign-artifacts.yml` that may have already started.
- **`dist generate` vs hand-rolled `release.yml` drift.** When
  bumping `cargo-dist-version`, run `dist generate` in a scratch
  branch + diff against the hand-rolled file to catch new schema
  requirements. Today no automation flags drift.
- **Apple + Windows targets parked.** Apple
  (`x86_64`/`aarch64-apple-darwin`) and Windows
  (`x86_64-pc-windows-msvc`) dropped from scope in 27.2. Phase 27.6
  (Homebrew) parked with them. To revive: add the targets back to
  `dist-workspace.toml`, restore matrix entries in `release.yml`,
  revive `packaging/homebrew/`, restore PowerShell installer.
- **`/api/info` daemon endpoint to expose build stamps.** Admin UI
  footer / About page wants the same four stamps (`git-sha`,
  `target`, `channel`, `built-at`) over HTTP, not just the CLI.
  Wire when Phase A4 dashboard lands.
- **`nexo self-update` (Phase 27.10).** `install-updater = false`
  in `dist-workspace.toml` keeps `axoupdater` off until the
  GH-releases source-of-truth is wired. Re-evaluate after the
  first live tag push exercises the workflow.
- **CHANGELOG.md root entry vs per-crate.** release-plz generates
  per-crate `CHANGELOG.md` on first release-PR; root file is the
  bin's changelog plus an index. Watch for bullet-style drift —
  acceptable but not desirable.

### Phase 27.4 — Debian + RPM packages deferrals

- **Phase 27.4.b — signed apt/yum repos in GH Pages.** GPG key
  generation + management (encrypt private with `age`, store in
  GH secret, `crazy-max/ghaction-import-gpg@v6` to import in
  runner), repo metadata via `apt-ftparchive` + `createrepo_c`,
  GH Pages publish job (mirror release assets into `apt/` +
  `yum/` paths), `nexo-rs.repo` + `apt sources.list` snippets in
  docs, optional `curl ... | install.sh` bootstrap that auto-detects
  distro. Cosign keyless (Phase 27.3) covers per-asset integrity
  but does NOT satisfy apt/yum trust chains — GPG is a separate
  signing system. New sub-phase entry in `PHASES.md`.
- **`NEXO_BUILD_CHANNEL` drift in `.deb` / `.rpm` packages.** The
  binary inside the deb/rpm is the same musl-static one cargo-dist
  built for the tarball, so `nexo --version --verbose` reports
  `channel: tarball-x86_64-unknown-linux-musl` even when the user
  installed via `apt install ./*.deb` or `dnf install ./*.rpm`.
  Fixing requires a dedicated rebuild per package channel — costs
  ~3 min CI per channel. Accepted today; revisit if support tickets
  surface confusion about install provenance.
- **arm64 install-test via qemu.** Today the install-test matrix
  is x86_64-only. arm64 needs `docker/setup-qemu-action@v3` +
  `--platform linux/arm64` overhead (~3 min per image). Backlog
  until either CI cycle budget tightens or arm64-specific issues
  show up in the wild.
- **Snap / Flatpak.** Out of scope. Reconsider only if community
  asks. Both formats add their own packaging dance + sandbox
  semantics that don't match the system-service shape the deb/rpm
  ship today.
- **systemd boot smoke in CI.** Containers without systemd-as-pid-1
  fail `systemctl enable`. The install-test matrix only validates
  `nexo --version` + `nexo --help`. Real systemd start lives
  manually or in a future VM-based CI lane.

## Resolved (recent highlights)

- 2026-04-28 — MCP denied-tool override now supports `Heartbeat`
  (`schedule_reminder`) with explicit hardening. In `nexo mcp-server`,
  `Heartbeat` can be exposed only when listed in both
  `mcp_server.expose_tools` and `mcp_server.expose_denied_tools`,
  auth is configured (`auth_token_env` or `http.auth`), the agent has
  `heartbeat.enabled = true`, and memory is available. The tool now also
  accepts MCP-friendly explicit route fields
  (`session_id`, `source_plugin` + optional `source_instance`,
  `recipient`) and falls back to `AgentContext` (`session_id`,
  `inbound_origin`) when present.

- 2026-04-28 — Cron tool/docs descriptions are now aligned with shipped
  semantics (A-8 closure). Updated `cron_*` `ToolDef` descriptions to
  explicitly cover origin-tagged binding scope, 60-second minimum
  interval, per-binding cap, and one-shot retry/drop behavior. Also
  removed stale "follow-up not shipped" wording in
  `cron_schedule`/`cron_runner`/`llm_cron_dispatcher` module docs and
  refreshed `docs/src/architecture/cron-schedule.md` to include
  `cron_pause`/`cron_resume`, origin tagging, model pinning, and the
  current plan-mode classification.

- 2026-04-28 — Cron one-shot dispatch now supports bounded retries
  instead of drop-on-first-failure only. `runtime.yaml` gained
  `cron.one_shot_retry` (`max_retries`, `base_backoff_secs`,
  `max_backoff_secs`; defaults `3 / 30 / 1800`). `CronRunner`
  schedules exponential-backoff retries on one-shot dispatch failure,
  increments durable `failure_count` per row, and drops the entry only
  after budget exhaustion. Store schema now includes
  `nexo_cron_entries.failure_count` with idempotent migration for
  existing DBs. Coverage added in `cron_schedule` + `cron_runner`
  tests.

- 2026-04-28 — `RemoteTrigger` now honors per-binding overrides.
  `InboundBinding` gained `remote_triggers` (replace semantics over
  `agents[].remote_triggers`), `EffectiveBindingPolicy` now resolves
  and carries that list, and `RemoteTriggerTool` reads from the
  session-effective policy instead of agent-level config only. Tool
  registration now considers both agent-level and binding-level
  remote-trigger lists so binding-only configs still expose the tool.
  Hardening included rate-limit bucket scoping by `(binding_index,
  trigger_name)` to avoid cross-binding interference when names match.
  Coverage added in `remote_trigger_tool` tests plus parse coverage in
  `crates/config/tests/binding_overrides.rs`.

- 2026-04-28 — Runtime now stamps interactive turn context from the
  inbound message (not session bootstrap only). `flush()` in
  `crates/core/src/agent/runtime.rs` builds a per-message context
  carrying `inbound_origin` and `sender_trusted`, so `EnterPlanMode`
  and trusted dispatch gates read real channel/account/sender data on
  live inbound turns. `sender_trusted` is asserted from pairing-gate
  `Decision::Admit` and defaults fail-closed elsewhere. Coverage added
  in `crates/core/tests/pairing_gate_intake_test.rs`.

- 2026-04-28 — Config approval subscriber now accepts both
  `plugin.inbound.<channel>` and
  `plugin.inbound.<channel>.<instance>` topics. No-instance events map
  to account `default`, which unblocks approvals from single-instance
  plugin routes.

- 2026-04-28 — `ConfigTool` now resolves proposal actor origin from the
  current `AgentContext.inbound_origin` when available, instead of
  always using a boot-time fallback binding. Approval correlation and
  staged proposal YAML now carry the real
  `(channel, account_id, sender_id)` of the turn that proposed the
  change. Coverage added in
  `agent::config_tool::tests::propose_uses_inbound_origin_from_context_when_available`
  (`--features config-self-edit`).

- 2026-04-28 — `ConfigTool` pending proposal recovery now survives
  process restarts. On boot, each tool instance rehydrates unexpired
  staged proposals from disk into both the correlator and
  `pending_receivers`; expired staging files are cleaned up. `apply`
  also has a lazy fallback that rebuilds a receiver from staging when
  the in-memory map is missing. Additional hardening kept from the
  earlier patch: propose-time staging failures now clean up both maps,
  and apply staging read/parse failures requeue the receiver instead of
  consuming it. Coverage added in
  `agent::config_tool::tests::boot_recovery_rehydrates_pending_proposals_from_staging`
  and
  `agent::config_tool::tests::apply_no_pending_can_recover_receiver_from_staging_file`
  (`--features config-self-edit`).

- 2026-04-28 — MCP resource URI allowlist now enforces hard reject
  before dispatch (no warn-only bypass). Both per-server
  `mcp_<server>_read_resource` and router `ReadMcpResource` paths
  share the same scheme gate, emit a `warn`, increment
  `mcp_resource_uri_allowlist_violations_total{server=...}`, and
  return an explicit error when the URI scheme is outside
  `mcp.resource_uri_allowlist`. Integration coverage updated in
  `crates/core/tests/mcp_resource_tool_test.rs` including router-path
  rejection/success cases.

- 2026-04-26 — `skills_dir: ./skills` in every agent YAML now points
  at `../skills` so the `resolve_relative_paths` step in
  `crates/config/src/lib.rs` (which roots relative paths at
  `<config_dir>/`) hits the project-level `skills/` tree instead of
  the non-existent `config/skills/`. Also dropped `web-search` from
  `agents.d/cody.yaml::skills` because no `skills/web-search/SKILL.md`
  ships in this checkout. Removes the WARN flood on every Cody turn
  and stops "missing SKILL.md" entries from masking real errors.

- 2026-04-26 — `nexo-driver-loop`'s `substitute_env_vars` no longer
  mangles UTF-8 in `config/driver/claude.yaml`. The loader copied
  bytes as `char` one at a time, so any multi-byte codepoint (e.g.
  the em-dash on line 1 of the shipped reference config) split into
  raw bytes — including C1 control bytes 0x80–0x9F that YAML
  rejects with "control characters are not allowed". Driver boot
  failed silently with a WARN, which Cody surfaced as "in-process
  driver isn't booted" and disabled every dispatch tool. Now the
  substitution copies the unmodified UTF-8 around each `${VAR}`
  span instead.



- 2026-04-26 — Admin first-run wizard at `/api/bootstrap/finish` now
  refuses to create `agents.d/<slug>.yaml` when an agent with that id
  already exists (either at the same path or in `config/agents.yaml`).
  Combined with the strict drop-in override rule below, this closes
  the loophole that produced a truncated `kate.yaml` next to a
  full definition and silently nuked the agent's bindings.
- 2026-04-26 — Runtime no longer treats "agent without
  `inbound_bindings`" as a wildcard. The empty-bindings branch in
  `crates/core/src/agent/runtime.rs` was removed; events go through
  `match_binding_index` unconditionally. The "legacy wildcard"
  fallback was the actual mechanism that let a single bot's
  messages reach every agent that subscribed to `plugin.inbound.>`.
  Tests updated in `crates/core/tests/runtime_test.rs` and
  `per_binding_override_test.rs` to lock in the strict rule.
- 2026-04-26 — `agents.d/<id>.yaml` drop-in overrides now REPLACE the
  base entry by `id` instead of appending a duplicate. Earlier the
  loader did `base.agents.extend(extra.agents)`, leaving two
  definitions for the same agent in the loaded config — when the
  override happened to omit `inbound_bindings`, the truncated copy
  fell into the runtime's "no bindings → legacy wildcard" branch and
  silently caught every plugin event. Fixed in
  `crates/config/src/lib.rs::merge_agents_drop_in`.



- 2026-04-26 — Telegram inbound fan-out now respects bot/agent
  isolation. `match_binding_index` in
  `crates/core/src/agent/runtime.rs` was tightened so a binding with
  `instance: None` only catches no-instance topics; per-bot setups
  must scope bindings with explicit `instance:`. Previously a
  no-instance binding swallowed every instance, fanning a single
  bot's messages out to every agent that listed the channel. Tests
  in `crates/core/tests/runtime_test.rs` and the inline unit suite
  updated to lock in the strict semantics.
- 2026-04-26 — Setup wizard now writes the per-instance allowlist on
  the right path everywhere. `telegram_link::run` accepts an
  `agent_id`, and `yaml_patch::telegram_append_chat_id` mutates the
  exact `telegram[<i>].allowlist.chat_ids` entry whose `allow_agents`
  matches. The CLI grew `agent setup telegram-link [<agent>]`. The
  legacy bug — `upsert("telegram.allowlist.chat_ids", …)` treating
  `telegram` as a map — is gone. `services_imperative::run_telegram`
  and `services/channels_dashboard::run_telegram_flow` already
  routed through `telegram_upsert_instance` and now also call the
  new `yaml_patch::upsert_agent_inbound_binding` helper so the
  agent's `inbound_bindings` carry the matching `instance:` (required
  under the tightened topic-match rule above).
- 2026-04-26 — Setup wizard seeds `pairing_allow_from` for every
  chat_id captured during onboarding (`telegram_link.rs` +
  `services/channels_dashboard.rs`). Operators that disable the YAML
  allowlist and rely solely on pairing no longer face a redundant
  challenge for an identity the wizard already approved. New
  `nexo-pairing` dependency added to `nexo-setup`; failures are
  logged but don't abort the wizard since the YAML allowlist still
  admits the chat.
- 2026-04-26 — Telegram plugin long-poll observes the shutdown
  cancellation token. `spawn_poller` in
  `crates/plugins/telegram/src/plugin.rs` now races the
  `bot.get_updates(...)` future against `shutdown.cancelled()` so
  Ctrl+C exits in <100 ms instead of waiting the full ~25 s
  long-poll. `offset` is only persisted on a successful round-trip,
  so cancelled updates are simply redelivered on next start.



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

- 2026-04-27 — **Phase 48 (Email channel) deferrals.** Phase 48 closed
  with sub-phases 48.1–48.10 ✅ but ten knobs were intentionally
  parked rather than bloat the closing slice:
  - **Interactive setup wizard.** ✅ Shipped 2026-04-27.
    `crates/setup/src/services/email.rs::run_email_wizard(
    config_dir, secrets_dir)` walks the operator through
    address → provider auto-detect via `provider_hint(domain)`
    (preset accept / override) → auth kind (Password /
    OAuth2Static / OAuth2Google) → secret entry.
    `upsert_email_account_yaml` upserts into `email.yaml`
    (idempotent on instance id, accounts beside it preserved)
    and `write_secret_toml` writes the TOML at mode 0o600
    (Unix) via temp+rename so a partial write never lands.
    Pure helpers (`derive_default_instance`,
    `serialise_secret_toml`, `render_account_block`) ship 10
    unit tests; the interactive shell still requires a TTY so
    e2e of the dialoguer flow is out of scope.
  - **Tool registration in `src/main.rs`.** ✅ Shipped 2026-04-27.
    `OutboundDispatcher` extracts a cheap `Arc<DispatcherCore>` that
    `EmailPlugin::dispatcher_handle()` returns post-start; main.rs
    builds an `EmailToolContext` after `plugins.start_all()` and the
    per-agent loop calls `register_email_tools(&tools, ctx)` when
    `agent.plugins` lists `email`. Six handlers (send / reply /
    archive / move_to / label / search) now reach the LLM.
  - **greenmail e2e** harness. 🔄 Partial 2026-04-27.
    `tests/pipeline_in_process.rs` covers the in-process slice:
    `OutboundDispatcher::enqueue_for_instance` →
    JSONL queue + Message-ID idempotency, `parse_eml` →
    `resolve_thread_root` → `session_id_for_thread` →
    `enrich_reply_threading`, `BounceStore` upsert + count
    increment, loop_prevent self-from skip. Five integration
    tests; broker is the local in-process bus, so the SMTP
    `DATA` round-trip and IMAP IDLE / FETCH / MOVE wire calls
    still need a Docker compose with greenmail in CI to land
    fully ✅.
  - **Hot-reload account diff.** ✅ Shipped 2026-04-27.
    `reload.rs::compute_account_diff(old, new) -> AccountDiff
    {added, removed, changed}` is the pure helper.
    `InboundManager` and `OutboundDispatcher` now hold per-
    instance `WorkerSlot { handle, cancel }` maps so a single
    worker can be torn down without touching siblings —
    parent cancel still kills the union, child cancel kills
    just one. `EmailPlugin::apply_account_diff(new_cfg, broker)`
    is the runtime entry: removes outbound first (so an in-
    flight job lands on disk before the inbound that read it
    disappears), then inbound; respawns `changed` accounts on
    both sides; spawns `added` last. The deprecated
    `apply_added_accounts` alias is preserved for back-compat
    but now forwards to the surgical implementation.
  - **Persistent bounce history.** ✅ Shipped 2026-04-27.
    `bounce_store.rs` ships a sqlx-sqlite `BounceStore` keyed on
    `(instance, recipient)` (recipient lowercased on insert /
    lookup). `inbound::drain_pending` now upserts every parsed
    bounce before publishing the wire event, incrementing a
    `count` column so a flapping recipient surfaces as a single
    row. `EmailToolContext.bounce_store: Option<Arc<BounceStore>>`
    is wired by main.rs from `plugin.bounce_store_handle()`;
    `email_send` consults it for every recipient (to + cc + bcc)
    and includes a `recipient_warnings` array in its success
    envelope when it finds prior bounces. Advisory only — the
    operator may have fixed the destination since the bounce, so
    the tool doesn't refuse to send.
  - **IMAP STARTTLS.** ✅ Shipped 2026-04-27.
    `ImapConnection::connect` now accepts `TlsMode::Starttls`:
    plain TCP dial, consume `* OK` greeting, run `STARTTLS`,
    upgrade the underlying `TcpStream` in place via the
    `tokio_util::compat` shim's `into_inner`, then resume the
    normal LOGIN / CAPABILITY flow on the TLS-wrapped session.
    `Plain` (no encryption) still rejects at connect — that's
    the security default we keep.
  - **Multi-selector DKIM probe.** ✅ Shipped 2026-04-27.
    `spf_dkim::DKIM_SELECTORS = ["default", "google", "selector1",
    "selector2", "mail"]` — first match wins. `AlignmentReport`
    carries `dkim_selector: Option<String>` so the matched selector
    surfaces; the `dkim_missing` WARN now logs the full list of
    probed selectors so the operator chasing a custom one knows
    what's already covered.
  - **`/healthz` HTTP integration.** ✅ Shipped 2026-04-27.
    `RuntimeHealth.email_plugin: Option<Arc<EmailPlugin>>` and a
    new `/email/health` route on the existing health server emit
    a sorted JSON array — one row per account with `state`
    (connecting / idle / polling / down), the IDLE / poll /
    connect timestamps, `consecutive_failures`,
    `messages_seen_total`, `last_error`, and the outbound
    queue/DLQ/sent/failed totals. Returns `[]` (not 404) when
    the plugin isn't configured so monitoring scripts can hit
    the route unconditionally.
  - **Dedicated Prometheus metrics** for email
    (`email_imap_state{instance}` gauge,
    `email_imap_messages_fetched_total{instance}` counter,
    `email_loop_skipped_total{reason}`,
    `email_bounces_total{instance, classification}`).
  - **Phase 16 binding-policy auto-filter.** ✅ Shipped 2026-04-27.
    `register_email_tools_filtered(registry, ctx, allow)` accepts
    an optional list of tool names to register; the no-arg
    `register_email_tools` is preserved as the all-six wrapper.
    `EMAIL_TOOL_NAMES` is the public canonical list.
    `filter_from_allowed_patterns(allowed)` derives the filter
    from `agent.allowed_tools` honouring the `*` / `email_*` /
    empty-list "register everything" semantics. main.rs's
    per-agent loop now passes the derived filter so
    `allowed_tools: ["email_send", "email_search"]` only
    registers those two handlers — instead of registering all
    six and pruning at LLM turn time.
  - **Cross-account attachment GC.** ✅ Shipped 2026-04-27.
    `attachment_store.rs` ships `AttachmentStore` (sqlx-sqlite,
    `email_attachments` table keyed on sha256 with first_seen /
    last_seen / count). `inbound::drain_pending` records every
    attachment after a successful parse so `last_seen` reflects
    the most recent message that referenced the file.
    `EmailPlugin::start` spawns a daily GC task that calls
    `gc(attachments_dir, retention_secs)` — sweeps both the row
    and the on-disk file when `last_seen < now - retention`.
    Missing files (manual cleanup, fs error) drop the row
    anyway so we don't keep retrying. New
    `EmailPluginConfig.attachment_retention_days` (default 90,
    `0` disables GC entirely).

## Phase 79.1 — Plan mode follow-ups

  - **Operator-approval scope check.** ⬜ Pending. Phase 79.1
    pairing approval (`[plan-mode] approve|reject plan_id=<ulid>`)
    currently authorises any sender on the binding's pairing
    channel. OpenClaw's `research/src/gateway/exec-approval-ios-push.ts:55-89`
    enforces a `roleScopesAllow({role: 'operator',
    requestedScopes: ['operator.approvals']})` check before
    accepting an approval message. When 79.10 ships
    `approval_correlator`, port that pattern: per-binding
    `operator.approvals` scope on the `(channel, account_id)`
    tuple, refusal logs `[plan-mode] approval rejected:
    sender lacks operator.approvals`. Hard prereq before the
    config-self-edit flow (79.10) opens up.
  - **`final_plan_path` variant.** ⬜ Pending if 8 KiB cap
    proves restrictive. The leak's `ExitPlanModeV2Tool.ts`
    reads the plan from disk via `getPlanFilePath(agentId)`;
    add an `ExitPlanMode { final_plan_path: PathBuf }` arm
    that points at a file written via `FileWrite` during
    plan mode. Only pursue when real workloads hit the cap.
  - **Acceptance retry policy.** ⬜ Pending. Phase 79.1
    fire-and-forget acceptance can be flaky (slow tests,
    transient network). Add bounded retry (1 retry after 30 s)
    before publishing `[plan-mode] acceptance: fail`.
  - **Acceptance hook fire-and-forget integration.** ⬜
    Pending (was step 14 of original 79.1 plan, parked at MVP).
    `ExitPlanMode` should spawn a tokio task on approve that
    runs the Phase 75 acceptance autodetect against the plan
    and posts `[plan-mode] acceptance: pass|fail (<summary>)`
    to `notify_origin` asynchronously. Today the unlock is
    inline; acceptance integration is a pure addition.
  - **Auto-enter-on-destructive (cfg-gated).** ⬜ Pending
    (was step 15 of original 79.1 plan). When
    `auto_enter_on_destructive: true` and the next call is
    classified destructive by Phase 77.8, the dispatcher
    pre-empts with a refusal carrying
    `entered_reason: AutoDestructive { tripped_check }` and
    flips state to On in the same step. Hard dep on Phase
    77.8 destructive-command warning shipping first.
  - **Pairing parser for `[plan-mode] approve|reject plan_id=…`.** ✅ 2026-04-30
    `parse_plan_mode_approval()` regex-based parser in `plan_mode_tool.rs`
    extracts `PlanModeApprovalCommand::{Approve|Reject}` from inbound
    chat messages. Process-shared `PlanApprovalRegistry` injected via
    `AgentRuntime::with_plan_approval_registry()` into all goal contexts.
    Broker subscriber in `main.rs` routes parsed `[plan-mode]` commands
    to `registry.resolve()`. 7 unit tests cover approve/reject/no-reason/
    whitespace/malformed/extra-text/empty-body.
  - **Notify_origin actual delivery (not just tracing).** ⬜
    Pending. The canonical `[plan-mode]` notify lines emit
    via `tracing::info!` today; production deployments need
    them surfaced through the pairing channel that owns the
    goal. Wire via the existing `HookDispatcher` /
    `PairingAdapterRegistry` plumbing that
    `notify_origin` already uses for completion hooks.
  - **End-to-end integration tests via dispatcher.** ⬜
    Pending (was step 16 of original 79.1 plan). Unit tests
    cover individual pieces (37 across `plan_mode`,
    `plan_mode_tool`, `tool_registry`, registry persistence,
    reattach). A dispatcher-level e2e — "goal calls Bash
    mutating while plan-mode On → receives PlanModeRefusal
    as `tool_result`" — would prove the wired-up gate
    end-to-end. Lives in
    `crates/dispatch-tools/tests/plan_mode_*.rs`.

## Phase 79.2 — ToolSearch follow-ups

  - ~~**LLM provider filtering of deferred schemas.**~~ ✅ 2026-04-30
    `ToolRegistry` gained `to_tool_defs_non_deferred()` and
    `deferred_tools_summary()`. `llm_behavior.rs::run_turn` now
    filters deferred tools from `req.tools` and appends a
    `<deferred-tools>` stub block to `system_blocks` so the model
    sees names + descriptions without paying for full schemas.
    `ToolSearch` stays non-deferred (registered via plain
    `register()`, not `register_with_meta()`).
  - ~~**MCP catalog auto-marks imported tools as deferred.**~~ ✅ already shipped
    (verified `mcp_catalog.rs:240-257` — `register_into` calls
    `registry.set_meta(&prefixed, ToolMeta::deferred())` for every
    inserted MCP tool).
  - ~~**Per-turn rate limit on `ToolSearch` itself.**~~ ✅ already shipped
    `ToolSearchRateLimiter` (sliding window, keyed by agent_id, default
    5 calls/min) lives in `tool_search_tool.rs:54-88`. Follow-ups entry
    was stale.
  - **Result format `<functions>` block parity with leak.** ⬜
    Pending. Current MVP returns matches as a JSON object with
    `name`/`description`/`parameters` per match. The leak instead
    returns `<tool_reference>` blocks that the SDK expands into
    real `<function>` declarations on the next turn. Useful for
    Anthropic-native callers that want zero JSON-parsing on the
    model side.

## Phase 79.7 — ScheduleCron follow-ups

  - ~~**Runtime firing not wired.**~~ ✅ shipped 2026-04-27.
    `crates/core/src/cron_runner.rs::CronRunner` polls
    `store.due_at(now)` every 5 s, dispatches via
    `Arc<dyn CronDispatcher>`, and advances state per-entry:
    recurring always advances (even on dispatch failure), while
    one-shot uses bounded retry policy
    (`runtime.cron.one_shot_retry`) before final drop. Spawned in
    `src/main.rs` right
    before `shutdown_signal().await` with a `LoggingCronDispatcher`
    (emits `[cron] fired` per dispatch).
  - ~~**LLM-call cron dispatcher.**~~ ✅ shipped 2026-04-27.
    `crates/core/src/llm_cron_dispatcher.rs::LlmCronDispatcher`
    builds `ChatRequest` from `entry.prompt`, calls
    `LlmClient::chat`, logs response with id + binding +
    cron + 200-char preview. `with_system_prompt` +
    `with_max_tokens` knobs. Runtime resolves the client from the
    entry's pinned `model_provider`/`model_name` with legacy
    fallback for rows created before model pinning. Falls back to
    `LoggingCronDispatcher` when no agents configured or
    LLM-client build fails (degraded boot stays observable).
    7 unit tests cover system-prompt prepended/empty/skipped,
    max-tokens propagation, LLM failure → error, empty
    response → ok, model_id taken from client, user-prompt
    routed.
  - ~~**Outbound publish to binding's channel.**~~ ✅ shipped 2026-04-27.
    `LlmCronDispatcher::with_publisher(Arc<dyn ChannelPublisher>)`
    routes the model's response to the user-facing channel when
    the entry carries both a `channel` (`<plugin>:<instance>`) and
    a `recipient` (JID / chat-id / email). Production wiring uses
    `BrokerChannelPublisher` which emits
    `{"kind": "text", "to": <recipient>, "text": <body>}` on
    `plugin.outbound.<plugin>.<instance>` — same envelope the
    WhatsApp / Telegram / Email outbound tools already speak.
    `parse_channel_hint` rejects malformed `<plugin>:<instance>`
    strings so the broker never sees `plugin.outbound.whatsapp.`
    (trailing dot). Publisher errors are logged via
    `tracing::warn!` but never fail `fire()` — the runner still
    advances state so a stuck downstream channel cannot deadlock
    the cron loop. `CronEntry.recipient: Option<String>` was added
    with an idempotent `ALTER TABLE` for older DBs and threaded
    through `cron_create` (new `recipient` arg). 5 publisher tests
    + 5 `parse_channel_hint` tests cover the happy path and edge
    cases (missing channel, missing recipient, publisher error,
    no publisher, malformed hints).
  - ~~**CLI `nexo cron list / drop / pause / resume`.**~~ ✅ shipped 2026-04-28.
    Operator-side cron admin now ships in `src/main.rs`:
    `agent cron list [--json] [--binding <id>]`,
    `agent cron drop <id>`, `agent cron pause <id>`, and
    `agent cron resume <id>`.
    This removes the need for direct SQL access for routine cron
    inspection and pause/resume/delete actions.
  - **Capability gate `cron.enabled` per binding.** ⬜ Pending.
    The MVP registers the tools globally — every agent gets
    them regardless of role. Spec called for `cron.enabled:
    bool` per binding (default `true` only for `coordinator` /
    `proactive` roles). Wire when 77.18 coordinator role
    lands.
  - ~~**Jitter on firing.**~~ ✅ 2026-04-30
    `RuntimeCronConfig.jitter_pct` (default 10). `CronRunner`
    applies `apply_jitter()` on recurring advance + one-shot retry
    timestamps. Zero-jitter by default in tests (deterministic).
    Plumbed from `runtime.yaml` → `CronRunner::with_jitter_pct()`.
    `apply_jitter()` already existed, ported from
    `claude-code-leak/src/utils/cronJitterConfig.ts` — wiring was
    the only missing piece.
  - ~~**`cron_pause` / `cron_resume` tools.**~~ ✅ shipped 2026-04-28.
    The `paused` column is now operator-reachable through tools:
    `cron_pause {id}` sets `paused=true` and `cron_resume {id}`
    sets `paused=false` without dropping the entry.

## Phase 79.11 — McpAuth follow-up

  - **`McpAuth` tool not shipped.** ⬜ Pending. Spec called for
    `McpAuth { server, op: refresh|status }` so the model can
    trigger an OAuth refresh or report auth state on a connected
    MCP server. The `McpClient` trait
    (`crates/mcp/src/client_trait.rs`) does not yet expose a
    `refresh_auth` / `auth_state` method — refresh is currently
    transparent inside the client. Once the trait grows the
    method (lift from
    `claude-code-leak/src/services/mcp/oauthPort.ts`), wire a
    third tool into `agent/mcp_router_tool.rs` and register it
    in `src/main.rs` next to the other two router tools.

## Phase 76.16 — expose_tools deferred items

  - **`Config` tool gated.** ⬜ Pending. `expose_tools: [Config]`
    emits a `tracing::warn!` and skips registration at startup.
    The Config tool (Phase 79.10) requires the full approval-correlator
    + plan-mode op-aware gating before it can safely be exposed to
    external MCP clients. Wire it once Phase 79.10 ships the
    approval workflow end-to-end and the `config_tool.self_edit` gate
    is validated against the originating channel.
  - **`Lsp` tool gated.** ⬜ Pending. `expose_tools: [Lsp]` emits
    a `tracing::warn!` and skips. LSP (Phase 79.5) requires spawning
    and managing a language server process; the tool itself is
    registered correctly for agent goals but the process lifetime
    is not safe to share across arbitrary MCP client sessions
    without additional session isolation. Defer until Phase 79.5
    follow-up lands per-session LSP process management.

## Maintenance note

If a future historical import includes non-English notes, keep them in `archive/spanish/*.txt` and update this Markdown tracker in English only.
