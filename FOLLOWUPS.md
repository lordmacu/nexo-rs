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
- Missing: `agent_resilience::CircuitBreaker` wrap around the
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

L-1. **Telemetry counters for link fetches**
- Missing: `link_understanding_fetch_total{result}` (ok / blocked /
  timeout / non-html / too-big), `link_understanding_cache_total{hit}`
  (true / false), and a per-fetch latency histogram. Doc page in
  `docs/src/ops/link-understanding.md` already advertises "no
  telemetry counter wired yet".
- Why deferred: feature is opt-in and currently unused in any
  shipped agent; metrics surface area can grow once a real workload
  exercises it.
- Target: alongside Phase A4 admin-ui dashboard work, or earlier if
  an agent enables `link_understanding` in production.

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
