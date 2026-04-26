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
