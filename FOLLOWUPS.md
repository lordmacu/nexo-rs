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

H-1. ~~**CircuitBreaker missing on Telegram + Google plugins**~~  🔄 partial 2026-04-26
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

PR-4. ~~**Companion-tui not shipped**~~  🔄 partial 2026-04-26
- ~~Reference scaffold shipped~~ as `crates/companion-tui` with
  binary `nexo-companion`. Decodes the base64url setup-code
  payload `nexo pair start` emits, validates the embedded
  bootstrap-token expiry, and prints the URL + redacted token
  + next-step instructions in pretty or `--json` form.
- Reads the code from `--code <BASE64URL>` arg or stdin (so
  QR-scan tools can pipe their output without quoting).
- 4 unit tests cover the redactor (short → placeholder, long
  → head + tail with `…` middle), and the next-step text
  builder (expired vs active branches).
- **Still deferred**: the actual WebSocket handshake against
  `payload.url`, presenting the bootstrap token in the first
  frame, receiving + persisting a session token. The scaffold
  exercises the wire format end-to-end so the protocol
  contract is enforced; the WS layer is its own follow-up
  (PR-4.x).
- Why ship the scaffold first: third parties asked how to
  integrate; pointing them at this binary as a reference is
  cheaper than maintaining a side-doc that drifts out of
  sync with the actual `setup_code` schema.

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
- **Still deferred**: `ws_cleartext_allow` is parsed but not
  yet plumbed into `pair start`'s URL resolver — the resolver
  takes its `extras` list from a different code path. One-step
  rewire when 26.x reaches the URL resolver again.

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

PT-4. **`HookIdempotencyStore` not consumed by
`DefaultHookDispatcher`**
- Missing: the SQLite store ships in Phase 67.F.3 but the hook
  dispatcher does not yet wrap firings in
  `try_claim` → side effect → ok. Without it, an at-least-once
  NATS replay or a daemon restart between "decide hook fires"
  and "side effect committed" can fire a hook twice.
- Why deferred: the wrap needs the dispatcher to take a store
  reference and decide pre- vs post-side-effect claim per
  action kind. Out of scope for the F.3 step (which only
  shipped the persistence primitive).
- Target: hardening pass post-PT-1.

PT-5. **Single-flight cap-counting race in `AgentRegistry::admit`**
- Missing: `admit` reads `count_running()` then writes; under
  concurrent dispatch the cap can be overshot by N where N =
  number of in-flight admits.
- Why deferred: low-impact in practice (cap is a soft hint and
  the orchestrator's per-goal worktree contention surfaces the
  real ceiling); a Mutex over the cap counter would serialise
  every admit.
- Target: when contention shows up in the multi-agent e2e
  benchmark.

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

PT-9. **Non-chat origin discriminator hardcoded as 'console'**
- Missing: `notify_origin` short-circuits when
  `origin.plugin == "console"` so CLI dispatch never publishes a
  chat reply. Future non-chat origins (`cron`, `webhook`,
  `heartbeat`) will need the same opt-out.
- Why deferred: only `console` exists today. Cleanest fix is a
  trait-based `OriginAdapter::wants_notify()` so each plugin
  declares its own behaviour, but it isn't worth the surface
  change before a second non-chat plugin lands.
- Target: when a second non-chat origin (cron / webhook / etc.)
  appears.

PT-8. **Multi-agent end-to-end test not shipped**
- Missing: a single integration test that wires
  orchestrator + registry + dispatch-tools + a mock
  pairing-adapter, dispatches two goals concurrently, and
  asserts a `notify_origin` summary lands on the mock adapter
  for each.
- Why deferred: the test needs the adapter wiring (PT-1) so
  the chat origin propagates into the hook payload.
- Target: alongside PT-1 / PT-3 / PT-4.

## Resolved (recent highlights)

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

## Maintenance note

If a future historical import includes non-English notes, keep them in `archive/spanish/*.txt` and update this Markdown tracker in English only.
