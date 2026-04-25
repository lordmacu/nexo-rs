# Phases

This file is the **English roadmap index**.

Historical phase-by-phase detailed notes that were previously written in Spanish are preserved at:
- `archive/spanish/PHASES.es.txt`

## Status

Implementation complete through Phase 20. Phases 21-26 are the
prioritised backlog of OpenClaw-parity work surfaced by the cross-
project audit (see `audits/openclaw-gap-analysis.md` if/when
written).

## Completed roadmap

1. Phase 1 — Core Runtime
2. Phase 2 — NATS Broker
3. Phase 3 — LLM Integration
4. Phase 4 — Browser CDP
5. Phase 5 — Memory System
6. Phase 6 — WhatsApp Plugin
7. Phase 7 — Heartbeat Scheduler
8. Phase 8 — Agent-to-Agent Delegation
9. Phase 9 — Production Polish
10. Phase 10 — Soul, Identity, Learning
11. Phase 11 — Extension System
12. Phase 12 — MCP Support
13. Phase 13 — Skills and External Integrations
14. Phase 14 — TaskFlow Runtime
15. Phase 15 — Claude Subscription Auth
16. Phase 16 — Per-Binding Capability Override
17. Phase 17 — Per-Agent Credentials
18. Phase 18 — Config Hot Reload
19. Phase 19 — Generic Poller Subsystem
20. Phase 20 — `agent_turn` Poller (scheduled LLM turns from YAML)

## Backlog — OpenClaw-parity phases

Each entry lists the gap surfaced by the comparison with `research/`
(OpenClaw reference impl) plus the proposed shape.

### Phase 21 — Link understanding   ✅

**Goal:** When a user message contains a URL, automatically fetch,
parse, and inject a summary block into the agent's context for that
turn — so agents stop saying "I can't see what's at that link".

Done criteria:
- `crates/core/src/link_understanding/` module: detect URLs (regex
  + Markdown autolinks), fetch via reqwest with size + content-type
  caps, extract main text via `readability`-style heuristic, render
  a `# LINK CONTEXT` system block.
- Per-agent toggle in `agents.yaml` (`link_understanding.enabled`,
  `max_links_per_turn`, `max_bytes`, `cache_ttl_secs`).
- Cache hits surface in telemetry; misses bypass on timeout.
- Recipe doc + opt-out for privacy-sensitive agents.

Reference: `research/src/link-understanding/`.

### Phase 22 — Slack + Discord channel plugins

**Goal:** Land two more inbound/outbound plugins so teams running
on Slack or Discord can adopt the agent without bridging through
WhatsApp/Telegram.

Done criteria:
- `crates/plugins/slack/` and `crates/plugins/discord/` with the
  same `Plugin` trait shape as whatsapp/telegram.
- Each declares `instance` config (multi-workspace), publishes
  `plugin.inbound.<plugin>.<instance>`, consumes
  `plugin.outbound.<plugin>.<instance>` for send-tools.
- Per-binding override (Phase 16) supports them out of the box.
- Per-agent credentials (Phase 17) extends to Slack OAuth + Discord
  bot token; gauntlet validates.
- Setup wizard entries (`agent setup slack` / `discord`).
- Outbound tools: `slack_send_message`, `slack_send_thread`,
  `discord_send_message`, `discord_send_dm`.

Reference: `research/extensions/slack/` and
`research/extensions/discord/`.

### Phase 23 — Realtime voice

**Goal:** Streaming STT (speech → text) → LLM → streaming TTS
(text → audio) loop, so an agent can hold a phone call or live
voice chat instead of just answering text.

Done criteria:
- New crate `crates/realtime-voice/` with provider-registry pattern
  (Deepgram, ElevenLabs, OpenAI Realtime, native browser MediaRecorder).
- Streaming pipeline: audio frames in → STT chunks → LLM
  `chat_stream` → TTS chunks → audio frames out, all bounded by a
  single CancellationToken.
- VAD (voice-activity detection) for interruption — barge-in
  semantics so the user can cut the agent off mid-reply.
- WebRTC bridge optional (call into Twilio / phone-control).
- Telemetry: end-to-end latency histogram (mic → first speech).

Reference: `research/src/realtime-voice/`,
`research/src/realtime-transcription/`, `research/src/tts/`.

### Phase 24 — Image generation provider abstraction

**Goal:** A single `image_generate` tool the LLM can call, backed
by a pluggable provider registry (OpenAI Images, Fal, Runway,
Comfy, Stable Diffusion via Replicate).

Done criteria:
- New crate `crates/media/` with provider trait
  `ImageGenerator: model + base_url + auth → image bytes`.
- Built-in providers for the common cases (start: OpenAI Images,
  Fal). Operators add custom via the same OpenAI-compatible slot
  pattern.
- Tool registers as `image_generate`; the agent attaches the
  resulting image to its outbound message via `Attachment`.
- Output goes through agent's outbound channel allowlist (no
  bypass).
- YAML config under `media.yaml` mirroring `llm.yaml`.

Reference: `research/extensions/fal/`,
`research/extensions/runway/`, `research/extensions/comfy/`,
`research/src/image-generation/`.

### Phase 25 — Auto-fetch web pages and search   ✅ (web_search shipped; web_fetch deferred — see follow-ups)

**Goal:** Make `web_search` and `web_fetch` first-class agent tools
backed by a provider registry — so any agent can search the web
without an extension.

Done criteria:
- `web_search` tool: queries via Brave / Tavily / Exa /
  SearXNG / Perplexity. Provider chosen via `web_search.provider`
  in agent config.
- `web_fetch` tool: existing `fetch-url` extension promoted to a
  built-in. Reuses Phase 21 link-understanding parser.
- Rate limit per agent + per provider.
- Search results indexed into vector memory for follow-up queries.

Reference: `research/src/web-search/`, `research/src/web-fetch/`,
`research/extensions/brave/`, `research/extensions/tavily/`.

### Phase 26 — Pairing protocol + companion app stub   ✅ (DM-challenge gate + setup-code CLI shipped; companion-tui deferred)

**Goal:** Replace ad-hoc `agent setup whatsapp` QR / token flows
with a pairing protocol that any companion app (CLI, mobile, web
UI) can drive. Sets up the foundation for native apps later.

Done criteria:
- `crates/pairing/` with setup-code generation, allow-from-store
  persistence, pairing-challenge handshake (Signal-Protocol-style
  X3DH-lite).
- New CLI: `agent pair start` (daemon emits a one-time code),
  `agent pair accept <code>` (companion claims it).
- One reference companion: a minimal web UI under
  `apps/companion-web/` (TypeScript, talks to admin endpoint via
  the paired token instead of loopback only).
- Documents the threat model (who sees the code, expiry, replay).

Reference: `research/src/pairing/`, `research/apps/`.

---

## Closing the gap to OpenClaw

The phases below are tracked from the
[honest gap analysis](docs/src/architecture/vs-openclaw.md). Each is
a real shortcoming today; landing them moves nexo-rs from
"technically better runtime" to "ship-it-tomorrow alternative".

### Phase 27 — Release pipeline + packaging

Today operators do `git clone + cargo build`. To compete with a
shipped product:

- GitHub Actions release workflow on tag push, fan out to:
  - signed tarballs (cosign / sigstore) for `linux-x86_64`,
    `linux-aarch64`, `macos-x86_64`, `macos-aarch64`
  - `.deb` + `.rpm` packages
  - Docker image at `ghcr.io/lordmacu/nexo-rs:<tag>` and
    `:latest`
  - Homebrew formula in a tap repo
  - Nix flake input + binary cache
  - Termux package recipe
- `cargo dist` integration to keep the workflow declarative.
- SBOM emitted per artifact.
- Reproducible-build attestation in CI.
- Release notes auto-generated from CHANGELOG.

Done when an operator can run *one* of:
`brew install nexo-rs`, `apt install nexo-rs`,
`docker run ghcr.io/lordmacu/nexo-rs`, `pkg install nexo-rs`
on Termux, `nix run github:lordmacu/nexo-rs`.

### Phase 28 — Production observability

Metrics exist (Prometheus); the operator-facing pipeline does not.

- Bundled Grafana dashboards under `ops/grafana/` covering:
  agents up, LLM latency p50/p95/p99, broker lag, DLQ depth,
  TaskFlow status counts, capability toggles armed.
- OpenTelemetry traces with W3C context propagation across NATS
  hops (`event.tracing` field carries traceparent).
- OTLP exporter wired behind `runtime.observability.otel.endpoint`
  so traces ship to Jaeger / Tempo / Honeycomb without a code
  change.
- `agent metrics --serve` standalone subcommand (already exposed
  on the admin port; phase fixes scrape config + service
  discovery hints).
- Cost tracking: per-agent / per-binding / per-session token
  aggregation table (rolling 24h + 30d) exposed via
  `agent costs` CLI and a `/api/costs` admin endpoint.

Done when an operator deploys, points Grafana at our
data-source, and sees the dashboards populate without writing a
single PromQL query.

### Phase 29 — Admin UI completion (A3 → A11)

Phase A1 (wizard) and A2 (channels Telegram CRUD) are done. The
remaining surfaces:

- **A3 — Agent configuration** — Identity / Soul / Brain / Tools
  / Channels / Memory / Skills / Delegation / Dreaming /
  Workspace / Danger zone tabs.
- **A4 — Runtime dashboard** — agent cards, throughput, error
  rate, breaker state, DLQ inspector with replay, TaskFlow
  explorer with manual resume, logs tail (SSE), Prometheus
  scrape preview.
- **A5 — Hot-reload one-click** — diff preview against current
  snapshot, apply with rollback button.
- **A6 — Capabilities tab** — surfaces `agent doctor capabilities
  --json`, paste-ready export blocks, risk badges.
- **A7 — Skills editor** — per-agent `skill_overrides` picker,
  `bin_versions` constraint editor with live probe.
- **A8 — Transcripts panel** — FTS search UI, redaction toggle,
  reindex trigger.
- **A9 — Secrets panel** — 1Password inject command allowlist
  editor, audit log JSONL tail with filters.
- **A10 — TaskFlow operations** — live `Waiting` flow list,
  manual resume / cancel, knob editor for `tick_interval` and
  `timer_max_horizon`.
- **A11 — RBAC + multi-user** — admin sessions tied to roles,
  not just "the password". Read-only auditor role, admin role,
  owner role.

Done when every config knob the daemon understands has a UI
toggle (no operator needs to edit YAML by hand for a routine
change), and the admin server passes basic OWASP ASVS L1.

### Phase 30 — Companion apps (Flutter)

OpenClaw ships iOS / Android / macOS companions. Without an
equivalent, nexo-rs is "a daemon you SSH into".

**Stack decision:** Flutter (Dart) for one codebase that targets
iOS + Android + macOS + Linux + Windows + Web. Trade-off:
platform-specific bridges (iMessage on iOS, share intents on
Android, menu-bar on macOS) ship as Flutter platform channels
calling thin native code, but the chat surface, settings, and
admin views are 100% shared. This loses some native polish vs
SwiftUI/Compose but gains 5 targets at the cost of one team.

Reference: how OpenClaw maps the surface — see
[docs → vs-openclaw](docs/src/architecture/vs-openclaw.md).

#### 30.1 — Gateway protocol on the daemon side

Mobile apps need a stable transport that doesn't go through the
admin HTTP server (admin is operator-only, loopback-bound). A
new `Gateway` server runs alongside the agent runtime:

- New crate `crates/gateway/` exposing a WebSocket endpoint
  (`wss://<host>:18789` by default).
- Frame format: `RequestFrame` / `ResponseFrame` / `EventFrame`
  in JSON-RPC 2.1 shape, schemas validated by `serde` + a
  `gateway-schema.json` shipped under `docs/schema/`.
- Methods (mirrored from OpenClaw's `validateXxxParams`):
  `Connect`, `Send`, `Poll`, `Wake`, `Agents.{list,create,update,
  delete}`, `Agents.files.{list,get,set}`, `MessageAction`,
  `Commands.list`, `NodePair.request`.
- Capability-scoped tokens: token carries a bitmask of allowed
  methods; `auth-install-policy.rs` resolves at handshake.
- Bind modes: `loopback` (default safe), `lan`, `tailscale`
  (auto-resolve via `tailscale status`), `tunnel` (Cloudflare).

#### 30.2 — Bootstrap pairing

Pairing flow as in OpenClaw `src/pairing/`:

- `agent pair start [--label "kate's iphone"]` on the daemon
  emits a one-time setup code (6-digit numeric) + the gateway
  URL the device should connect to.
- The device sends `NodePair.request { code, device_info }` and
  receives a long-lived `bootstrapToken` + capability scope.
- Tokens stored in a new `pairing-store` table in
  `data/gateway.db`; the daemon-side store carries
  `(device_id, token_hash, label, scopes, created_at, last_seen)`.
- Per-device `allow-from-store` table: which channels each token
  may operate on (e.g. "ana's phone can send to WA but not TG").
- Token revocation: `agent pair list / revoke <id>`.
- Threat model documented under
  `docs/src/architecture/pairing-threat-model.md`.

#### 30.3 — Flutter shell + shared protocol client

Single codebase under `apps/flutter/`:

- `dart packages/nexo_protocol/` — generated Dart types from the
  same `gateway-schema.json` so the protocol doesn't drift.
- `apps/flutter/lib/transport/` — WebSocket client with
  reconnect, heartbeat (`Wake`), and binary frame compression.
- `apps/flutter/lib/screens/`:
  - `chat/` — per-agent chat surface, send + media + voice notes
  - `agents/` — list, edit (SOUL/MEMORY), per-agent toggles
  - `events/` — live timeline of every inbound/outbound event
  - `approvals/` — destructive-action approval queue with
    biometric gate (`local_auth` package)
  - `pairing/` — paste setup code + scan QR variant
  - `settings/` — gateway URL, manage devices, notifications
- State management: `riverpod` (community standard 2025).
- Persistence: `drift` (SQLite over Flutter) for the message
  cache + pairing token in OS keychain (`flutter_secure_storage`).

#### 30.4 — Platform-specific bridges

Each native bridge is a thin Flutter platform channel that
delegates to a Swift / Kotlin / Objective-C method:

- **iOS** (`apps/flutter/ios/Runner/`):
  - `ShareExtension` — accept text/image/url shared from any
    app, post via `Send` to a configured agent.
  - `NotificationServiceExtension` — render encrypted push
    payload bodies (operator approves "agent paid invoice
    $X").
  - `WatchKit` companion — at-a-glance recent events.
  - **iMessage bridge** (privileged): a method-channel that
    polls `chat.db` if the user grants Full Disk Access,
    pushes to the daemon. Optional, off by default.
- **Android** (`apps/flutter/android/app/`):
  - Foreground service to keep the WS open under doze.
  - `ShareTarget` intent filter for the share sheet.
  - Direct-Reply notification action.
  - **SMS bridge** (with explicit READ_SMS / SEND_SMS perms):
    optional, off by default.
- **macOS** (`apps/flutter/macos/Runner/`):
  - Menu-bar item via `system_tray` package showing agent
    status + recent events.
  - Spotlight integration to launch chats by agent id.
- **Linux / Windows** — Flutter desktop bundles, no special
  bridges, just the chat shell.
- **Web** — same Flutter codebase, served from a static path on
  the gateway. Acts as the PWA fallback for users who don't
  want to install.

#### 30.5 — Distribution

- iOS via TestFlight first, App Store later (privacy review
  needed for share extension + iMessage bridge).
- Android via Play Internal Test → public; also
  `apps/flutter/build/app/outputs/flutter-apk/app-release.apk`
  for sideload.
- macOS notarized DMG via Fastlane.
- Windows MSIX, Linux AppImage / Flatpak — secondary.
- Web build deployed to GitHub Pages alongside the docs as
  `https://lordmacu.github.io/nexo-rs/app/`.
- All releases tied to Phase 27's release pipeline.

Done when:

1. An operator runs `agent pair start`, opens the Flutter app
   on iOS or Android, types the code, and the device shows up
   in `agent pair list` with the right scopes.
2. The operator can chat with an agent end-to-end from the
   phone, edit the agent's `SOUL.md` from the app, and approve
   a destructive action from a push notification.
3. Sharing a URL from another app via the share sheet posts to
   a configured agent on both iOS and Android.
4. The same Flutter build runs as a PWA at the gateway URL with
   fallback to polling when WebSocket isn't usable.

Reference: `research/src/gateway/`, `research/src/pairing/`,
`research/apps/`. The Flutter choice diverges from OpenClaw's
SwiftUI / Compose split; OpenClaw maintains separate native
codebases. We optimize for ship velocity over native fidelity.

### Phase 31 — Plugin marketplace + discoverability

Today nexo-rs's 30 extensions live in this repo. There is no
external author story.

- `agent ext install <name>` resolves against a registry index
  (a static JSON file at `https://lordmacu.github.io/nexo-rs/ext-index.json`
  or a dedicated repo).
- Each registry entry carries: `id`, `version`, `download_url`,
  `sha256`, `manifest_url`, `signing_key`, `homepage`,
  `min_runtime_version`.
- `agent ext install` verifies the signature against an
  allowlisted key set (`config/extensions/trusted_keys.toml`).
- A submission process: PR to the registry repo with a
  `manifest.toml` + signed tarball; CI builds and publishes.
- A "verified" tier (signed by us) vs "community" tier
  (third-party signed); operator chooses which to install.

Done when one third-party extension lives in the registry and
an operator installs it without cloning anything.

### Phase 32 — Multi-host orchestration

`FOLLOWUPS.md` P-2 promotes here once the gate is hit. Today
nexo-rs is single-host with NATS optional. To scale:

- A coordinator service (or NATS subject convention) that
  decides which host owns which agent / binding.
- Health checks + automatic failover when a host disappears.
- A `nexo-controller` binary (separate from `agent`) that runs
  the placement logic.
- Helm chart for Kubernetes deployment, with each agent as a
  named workload and the coordinator as a leader-elected
  StatefulSet.
- Load tests demonstrating failover within N seconds.

Done when a 3-node cluster recovers from a node kill within 30s
without operator intervention.

### Phase 33 — Trust & compliance signals

Soft-trust gaps that block enterprise adoption.

- **External security audit** — engage a third party (Trail of
  Bits, NCC, Doyensec). Publish report.
- **Bug bounty** — HackerOne / Intigriti listing with scope and
  rewards.
- **SOC 2 Type II readiness assessment** — gap analysis,
  remediation, annual review.
- **CVE process** — `SECURITY.md` already exists; add an
  embargoed disclosure timeline and an `advisory` repo for
  published CVEs.
- **Reproducible builds** — bit-for-bit identical artifacts from
  identical source.
- **Real-world deployment case studies** — at least three
  documented production users with metrics, scale, uptime.
- **Internationalization** — admin UI under `i18n/` JSON
  catalogues; `agent admin --lang es` works end-to-end.

Done when the project page can credibly link to an external
audit report, a public bug bounty, and three production case
studies.

### Phase 34 — Cross-cutting hardening parked from audits

Tracking real fixes the two audit passes left parked:

- **H-1 (FOLLOWUPS.md)** — Telegram + Google CircuitBreaker
  with a per-instance vs per-agent scoping decision.
- **A-H2 (audit pass 2)** — server-side cookie revocation list.
- **A-M3** — tunnel `Drop` watchdog so `cloudflared` can't
  zombie when the parent dies abnormally.
- **WA-H2** — outbound dispatcher producer-queue refactor so a
  slow URL doesn't pin the whole channel.
- **MCP-M1** — per-server priority for tool name collisions.
- **WA-M2** — daemon collision flock around the WhatsApp
  session dir.
- **WA-M3 / WA-M4** — reactive→proactive race fix and
  `MediaReceived` published before download completes.
- **B-M1 / B-M2** — multi-row drain transaction + DLQ replay
  preserves attempt history.

Each of these is bounded but bigger than a one-line fix.
Roll them up into a single hardening sprint when the next
audit is run.

### Phase 35 — Performance + benchmarks

Today the README claims "Rust faster" without numbers. To either
back it up or find regressions:

- A `bench/` crate using `criterion` for hot paths:
  broker publish/subscribe throughput, LLM stream parse,
  TaskFlow tick latency, transcripts FTS search, redaction
  pipeline.
- End-to-end load test rig: spawn N inbound messages over local
  broker, measure tail latency under different agent counts.
- Memory profiling: `dhat` or `bytehound` snapshots at idle and
  under load; document RSS at 1, 10, 100 sessions.
- Compare against OpenClaw on equivalent workload (same prompt,
  same provider) and publish the table.
- CI gate: criterion regression detection on PRs that touch the
  hot path.
- Profile-guided optimization (`-Cprofile-use`) for the release
  binary — re-evaluates after first measured baseline.

Done when `docs/src/bench/` carries reproducible numbers + the
README has a "performance" section that isn't aspirational.

### Phase 36 — Backup, restore, migrations

The agent owns persistent state across multiple SQLite DBs
(`memory.db`, `taskflow.db`, `transcripts.db`, `data/`). No
operator-facing backup/restore shipped.

- `agent backup --out <dir>` — atomic snapshot of every DB +
  `config/` + `secrets/` (skipped or encrypted), tarballed and
  manifest-hashed.
- `agent restore --from <archive>` — reverses, verifies hashes,
  refuses if running.
- `agent migrate up|down|status` — proper schema migrations with
  versioning replacing the current `ALTER TABLE … .ok()`
  pattern. Each migration has a rollback path.
- Automated daily backup cron via systemd timer / `termux-job`.
- Encryption at rest option: `--passphrase` flag, age-based.

Done when an operator can `agent backup` on host A and
`agent restore` on host B, end up with the same agent state, and
audit a clean migration log.

### Phase 37 — Plugin author DX

Lower the barrier to writing extensions so the ecosystem isn't
just whoever speaks Rust.

- `nexo-plugin-sdk` TypeScript package (`packages/plugin-sdk-ts/`)
  mirroring the Rust SDK shape. Stdio JSON-RPC + manifest +
  capability declarations. Targeted at plugin authors who write
  Node/Bun.
- `nexo-plugin-sdk` Python equivalent.
- `agent ext init <name> --lang <rust|ts|py>` scaffold with a
  working tool, manifest, README, CI workflow template.
- A long-form tutorial under `docs/src/recipes/build-a-plugin.md`
  walking from `agent ext init` → registry submission.
- Plugin author cookbook: rate limiting, secrets handling,
  testing harness, telemetry conventions.
- Sandbox option: `transport: wasm` in `plugin.toml` — extensions
  ship as a `.wasm` module; host runs them with `wasmtime` or
  `wasmer` in a capability-restricted sandbox. Removes the
  "trusted code" question from third-party plugins.

Done when one external author publishes a working extension in a
non-Rust language using nothing but the SDK + tutorial.

### Phase 38 — Chaos + fuzzing + property tests

Audit found two hidden race classes (header race, FTS rebuild
race). The next ones are still in there.

- `cargo fuzz` targets for: protocol decoders (NATS subject
  parser, MCP JSON-RPC, SSE event parser), redaction patterns,
  config YAML loader, FTS5 query escape.
- `proptest` / `quickcheck` for invariants: round-trip
  serialization, idempotent `apply` on `Redactor`, monotonic
  `next_id` under contention.
- Chaos test rig: a tokio test that randomly kills the broker,
  injects partial writes, drops messages, asserts the agent
  recovers within bounded time.
- `loom` or `shuttle` runs over the broker drain path, the
  taskflow wait engine, and the runtime reload path.
- CI integration: fuzz overnight on `main`, file findings as
  issues automatically.

Done when fuzz harnesses have run for 24h+ on `main` without a
new finding and the chaos rig is part of the release gate.

### Phase 39 — Stable admin API contract

The admin server's HTTP API is whatever the SPA needs today. To
let third parties build against it:

- `agent admin --openapi` emits an OpenAPI 3.1 spec.
- Versioned routes: `/api/v1/...`. Breaking changes go to
  `/api/v2/...` with a deprecation window on v1.
- Schema validation: every handler validates request bodies
  against the spec (server-side) and emits typed errors.
- Generated TypeScript client under `packages/admin-client-ts/`
  bundled with the SPA but reusable by any TS consumer.
- Python + Rust generated clients for completeness.
- E2E test suite: spec-driven contract tests so API drift fails
  CI.
- WebSocket / SSE channels for live state (agents, DLQ, taskflow)
  so polling isn't required.

Done when `npm install @nexo-rs/admin-client` works and the spec
is the single source of truth for both server and clients.

### Phase 40 — Deployment recipes

Today docs cover Termux + bare Linux. Most operators land on a
cloud VM and need a recipe.

- `docs/src/recipes/deploy-aws.md` — EC2 t4g.small + EBS +
  Route53 + ACM + ALB + IAM role for SES; estimated cost.
- `docs/src/recipes/deploy-gcp.md` — same shape with Compute
  Engine + Cloud SQL (or local SQLite) + Identity-Aware Proxy.
- `docs/src/recipes/deploy-hetzner.md` — CX22 with Ubuntu, full
  systemd + cloudflared + UFW + automatic upgrades.
- `docs/src/recipes/deploy-fly.md` — fly.toml + persistent
  volume + Tigris S3 backups.
- `docs/src/recipes/deploy-render.md`, `deploy-railway.md`,
  `deploy-vercel.md` (where applicable).
- A `deploy/terraform/` module per cloud so an operator can
  `terraform apply` once and have a working setup.
- A `deploy/k8s/` set of manifests + Helm chart (depends on
  Phase 32 multi-host).

Done when an operator picks a cloud, follows the recipe, and is
in a healthy state with HTTPS within an hour.

### Phase 41 — Telemetry opt-in + roadmap signal

Honest version of "we don't know what people use".

- `agent telemetry status|enable|disable` CLI knob.
- Anonymous metrics emitted weekly to a central endpoint:
  agent count, channel mix, LLM provider mix, average session
  count, version. **No** message content, **no** identifiers, no
  IPs (server strips at ingress).
- Privacy policy + telemetry doc explaining exactly what is sent.
- Public dashboard at `https://lordmacu.github.io/nexo-rs/usage/`
  showing aggregate adoption.
- Default: **disabled**. Opt-in only. Banner at first launch
  explaining the trade.

Done when the privacy policy is published and operators can opt
in/out without restarting.

### Phase 42 — Internationalization

Admin UI is English-only. Agents talk multiple languages already
(per-binding `language:`), but the operator surface doesn't.

- `i18n/` JSON catalogues: `en.json`, `es.json`, `pt.json`,
  starting with the wizard and admin UI strings.
- `agent admin --lang <code>` flag + `Accept-Language` honored
  by the server.
- mdBook docs duplicated under `docs/src/es/` for Spanish (the
  primary non-English audience based on the operator's own
  language).
- Translation contribution guide in `docs/src/contributing.md`.

Done when an operator picks Spanish and the entire admin UI
flips, including error messages.

### Phase 43 — Real-world case studies

Ship-it credibility — without these, the comparison tables read
as theoretical.

- At least three documented production deployments with:
  agent count, channels in use, monthly inbound/outbound volume,
  uptime over 90 days, incident retro if any.
- Each case study is a markdown page under
  `docs/src/case-studies/<name>.md` with the operator's
  consent.
- A "users" page listing the orgs (with their permission) and
  linking the case studies.
- A short interview / video for at least one of them.

Done when the README's "Used by" section is populated with real
logos / handles, and three full case studies are linked.

### Phase 44 — Auxiliary observability surfaces

Smaller observability gaps the main phases don't cover.

- Structured event log per session under `data/events/<session>.jsonl`
  for forensics, separate from transcripts.
- `agent inspect <session_id>` — pretty-print every state
  transition for one session: tool calls, hook fires, broker
  publishes, memory writes, redaction hits.
- `agent doctor health` — single command that runs every doctor
  (`setup`, `ext`, `capabilities`) and emits one health summary
  for monitoring scrapers.
- Standard k8s `/healthz` and `/readyz` endpoints documented.
- Crash dumps captured to `data/crashes/` with stack + recent
  log buffer.

Done when an operator triaging an incident has one command per
question and a deterministic place to look for breadcrumbs.

### Phase 45 — Cost & quota controls

LLM token usage is logged. The operator can't act on it.

- Per-agent monthly budget cap (`agents.<id>.cost_cap_usd`).
  Once hit, the agent stops accepting new turns and a warn fires
  on a configurable broker subject.
- Per-binding token rate limits (e.g. "sales WA capped at 5k
  tokens/hour") on top of the existing `sender_rate_limit`.
- A cost prediction tool: pre-flight token counter (already
  partly in `nexo_llm::TokenCounter`) drives an estimate that
  the agent can include in its system prompt ("you have 80% of
  budget remaining today").
- `agent costs` CLI: rolling 24h, 7d, 30d, by agent / binding /
  provider / model.
- Alerts (broker subject + admin notification) on configurable
  thresholds (`> 80% of cap`, `> p95 latency`, etc.).

Done when an operator can put a hard ceiling on every
agent's monthly LLM bill and get notified before it's hit.

---

## Deliberately NOT roadmapped

These OpenClaw features were considered and deferred — listing them
keeps the door open without committing scope.

- **Canvas-host / per-agent web UI** (`research/src/canvas-host/`):
  large UX surface area; hold until Phase 26 pairing lands so a
  companion app can host it.
- **Proxy-capture** (`research/src/proxy-capture/`): valuable for
  debugging extensions but niche; revisit when we hit a real
  observability gap that can't be solved with logs.
- **Auto-reply orchestrator** (`research/src/auto-reply/`):
  OpenClaw built a 50-file system because TS lacked our debounce
  +  per-binding plumbing. Our equivalents are already in core +
  Phase 16; reach for individual primitives only as concrete needs
  surface.
- **Scattered channel plugins** (Matrix, IRC, iMessage, Line, QQ,
  WeChat, Synology, Tlon, …): handled case-by-case under Phase 22
  follow-ups, not as their own phases.

## Current working mode

- New work is tracked as follow-ups and hardening tasks.
- Active backlog lives in `FOLLOWUPS.md`.
- Architecture remains documented in `design-agent-framework.md`.

## How to add a new phase

When a new major implementation phase is introduced:

1. Add the phase title and objective here.
2. Add explicit done criteria.
3. Add the implementation checklist in English.
4. Link follow-up debt entries in `FOLLOWUPS.md`.
5. Update mdBook docs in the same commit.

## Documentation policy

- Keep Markdown documentation in English.
- Keep historical non-English material only in `archive/spanish/*.txt`.
