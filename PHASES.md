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

#### 26.x — Pairing challenge reply via adapter   ✅

- `PairingChannelAdapter` extended with default
  `format_challenge_text` and a default `bail!`-ing
  `send_qr_image` so plugins only override what they need.
- `PairingAdapterRegistry` added in `nexo-pairing` and re-exported.
- `PairingGate::should_admit` accepts an
  `Option<&dyn PairingChannelAdapter>` so the canonical sender form
  (e.g. WA `+digits` after `@c.us` strip, TG `@username` lower-case)
  is used for both store lookup and cache key.
- WhatsApp + Telegram plugins ship `WhatsappPairingAdapter` /
  `TelegramPairingAdapter`; Telegram escapes MarkdownV2 reserved
  chars so the pairing code renders as inline code.
- Runtime gained `with_pairing_adapters(reg)`; on
  `Decision::Challenge` it delegates to the registered adapter and
  falls back to a hardcoded broker publish for unregistered
  channels.
- New counter `pairing_inbound_challenged_total{channel,result}`
  with results `delivered_via_adapter`, `delivered_via_broker`,
  `publish_failed`, `no_adapter_no_broker_topic`.
- Bin (`src/main.rs`) wires the WA + TG adapters at boot.
- Direct in-process `Session::send_text` delivery (skipping the
  broker entirely) remains deferred — adapters publish via broker
  too in this pass.

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

#### 26.y — Pairing telemetry counters (rest)   ✅

Tracks **PR-2** in `FOLLOWUPS.md`. Wire the remaining counters
the spec called for, on top of `pairing_inbound_challenged_total`
already shipped in 26.x:

- `pairing_requests_pending{channel}` (gauge)
- `pairing_approvals_total{channel,result}`
- `pairing_codes_expired_total`
- `pairing_bootstrap_tokens_issued_total`

Consumer: admin-ui Phase A4 dashboard.

#### 26.z — `tunnel.url` integration in URL resolver   ✅

`nexo pair start` should consult a fixed priority chain when
resolving the public URL it embeds in the QR/setup-code payload.
Most of the chain already shipped under PR-3 in `FOLLOWUPS.md`
(see that section for the bullet-by-bullet history); this
sub-phase verifies the wire-up end-to-end and lands the missing
sidecar precedence.

Concrete done criteria:

1. `crates/pairing/src/url_resolver.rs` (or equivalent module —
   create one if missing) exposes `resolve_public_url(opts) ->
   Result<String, ResolverError>` consulted by `run_pair_start`
   in `src/main.rs`. Priority order, top wins:
   1. `--public-url <URL>` CLI flag (already wired via clap).
   2. `pairing.yaml::public_url` (if `config/pairing.yaml`
      loader exists; otherwise leave the hook + TODO and fall
      through — do **not** invent a fake loader).
   3. `$NEXO_HOME/state/tunnel.url` sidecar via
      `nexo_tunnel::read_url_file()` (already shipped per PR-3).
   4. `NEXO_TUNNEL_URL` env var.
   5. Loopback `http://127.0.0.1:<port>` fallback (existing
      `pair_paths` behaviour, fail-closed when port unknown).
2. `ws_cleartext_allow` from `pairing.yaml` (when present) is
   threaded into the resolver `extras` so the cleartext-host
   allowlist can be set from YAML.
3. Unit tests in `crates/pairing/src/url_resolver.rs::tests`:
   - Each priority level overrides the levels below it.
   - Sidecar trim + idempotent absence (file missing → next
     fallback, no error).
   - Loopback fail-closed when no port supplied.
4. `nexo pair start --public-url https://override` smoke test
   (existing test or new) still passes — explicit flag wins.
5. `cargo build --workspace && cargo test --workspace` exits 0
   on the goal worktree.

Reference for shipped pieces: see PR-3 in `FOLLOWUPS.md` —
`url_state_path()`, `write_url_file()`, `read_url_file()`,
`clear_url_file()` in `crates/tunnel/`. Do **not** re-implement
those; just consume them.

#### 26.aa — `pair_approve` scope-gated agent tool   ⬜  (security review required)

Tracks **PR-5** in `FOLLOWUPS.md`. Built-in tool that lets agents
approve pending pairings from a trusted channel, scoped via
`EffectiveBindingPolicy::allowed_tools`. Opens prompt-injection
vectors — needs a clear trust model before landing.

#### 26.ab — `config/pairing.yaml` loader   ✅

Tracks **PR-6** in `FOLLOWUPS.md`. Move hardcoded paths
(`<memory_dir>/pairing.db`, `~/.nexo/secret/pairing.key`) and
`--public-url` to a `config/pairing.yaml` with `storage.path`,
`setup_code.secret_path`, `default_ttl_secs`, `public_url`,
`ws_cleartext_allow`. Also unblocks `nexo-tunnel` URL accessor
work in 26.z.

#### 26.ac — TaskFlow-backed companion-tui pairing   ⬜

When the deferred companion-tui lands, model the multi-step flow
(operator runs `nexo pair start` → QR shown → app scans → app
posts setup-code → server validates → session token issued) as
a `Flow` with `WaitCondition::ExternalEvent("pair.codes.{code}")`
between steps. Survives operator-side restart; the `WaitEngine`
wakes the flow when the app posts the code. Today the CLI blocks
on a synchronous spinner — fragile if the operator backgrounds
the process or the app takes >TTL to scan.

#### 19.x — Pollers V2 backlog   ⬜

Tracks **P-1**, **P-2**, **P-3** in `FOLLOWUPS.md`:

- **P-1** `inventory!` macro registry for built-in pollers —
  revisit when poller count > ~20.
- **P-2** Multi-host runner orchestration — covered by
  Phase 32; this entry is the cross-link.
- **P-3** Push-based watchers (Gmail Push, generic inbound
  webhooks) — likely its own crate / phase. Needs public TLS
  surface + inbound auth.
- **P-4** TaskFlow-backed batch polls — when a poll yields >100
  items (RSS dump, Gmail history sync after long offline), spawn
  a `Flow` that processes batches of N with cursor persisted per
  step. Resumes on crash; survives reboot. Today the runner just
  drops the cursor on panic. Low effort: `nexo-poller` already
  has `flow_manager` available via boot wiring.

#### 38.x — Test flakes & real concurrency races   ✅

Two unrelated flakes that surface under CI parallelism. Both fixed.

- **38.x.1** ✅ Fixed: extracted `render_into(ttft, chunks)` pure renderer.
- **38.x.2** ✅ Fixed 2026-04-29: TranscriptWriter header race — per-session
  `TokioMutex` around the `create_new` + header-write block. The
  `concurrent_first_appends_only_write_one_header` test is un-ignored
  and passes (0 flakes across 100+ local runs).

---

## Closing the gap to OpenClaw

The phases below are tracked from the
[honest gap analysis](docs/src/architecture/vs-openclaw.md). Each is
a real shortcoming today; landing them moves nexo-rs from
"technically better runtime" to "ship-it-tomorrow alternative".

### Phase 27 — Release pipeline + packaging

Today operators do `git clone + cargo build`. To compete with a
shipped product, every supported install path lands as a
shippable artifact on tag push.

#### 27.1 — `cargo dist` baseline   ✅

Shipped:
- `dist-workspace.toml` declares 6 targets (host-fallback
  `x86_64-unknown-linux-gnu` + the 5-entry shippable matrix:
  `x86_64`/`aarch64-unknown-linux-musl`, `x86_64`/`aarch64-apple-darwin`,
  `x86_64-pc-windows-msvc`). `precise-builds = true`,
  `installers = ["shell", "powershell"]`, `tag-namespace = "nexo-rs"`
  (matches `release-plz`'s `git_tag_name`), `allow-dirty = ["ci"]`,
  `install-path = "CARGO_HOME"`, `install-updater = false`.
- `[package.metadata.dist] dist = false` opt-out on every
  bin-bearing crate that should NOT ship in release tarballs:
  `nexo-driver-permission`, `nexo-driver-loop`, `nexo-dispatch-tools`,
  `nexo-companion-tui`, `nexo-mcp` (its `mock_mcp_server` is a test
  fixture). Root crate gains `[package.metadata.dist] dist = true`
  so only `nexo` ships.
- Dev / smoke programs (`browser-test`, `integration-browser-check`,
  `llm_smoke`) moved from `[[bin]]` to `[[example]]` under
  `examples/`. cargo-dist auto-skips examples, and they remain
  runnable via `cargo run --example <name>`. Makefile +
  `scripts/integration_nats_recovery.sh` updated to the
  `--example` form.
- `build.rs` emits four compile-time stamps consumed by
  `nexo version` (or `nexo --version --verbose`):
  `NEXO_BUILD_GIT_SHA`, `NEXO_BUILD_TARGET_TRIPLE`,
  `NEXO_BUILD_CHANNEL` (overridable via env;
  defaults to `source`), `NEXO_BUILD_TIMESTAMP` (UTC ISO8601).
  `chrono` added to `[build-dependencies]` (`default-features = false,
  features = ["clock"]`).
- New `Mode::Version { verbose }` in `src/main.rs`. Short form
  (`nexo --version` / `-V`) prints `nexo 0.1.1`. Verbose form
  (`nexo version` subcommand or `nexo --version --verbose`) prints
  the package version plus the four stamps. Inline unit test
  `tests::build_stamps_are_populated` asserts the env stamps are
  non-empty and `NEXO_BUILD_TIMESTAMP` is ISO8601 UTC.
- `scripts/release-check.sh` smoke gate validates whatever
  tarballs landed in `target/distrib/`: sha256 against `*.sha256`
  sidecars (when emitted), required contents (`nexo` /
  `nexo.exe`, `LICENSE-MIT`, `LICENSE-APACHE`, `README.md`), and a
  host-native extract + `nexo --version` regex match. Targets the
  local toolchain can't build emit `[release-check] WARN` instead
  of failing.
- `Makefile` gains `dist-build` (= `dist build --artifacts=local
  --tag nexo-rs-v$(NEXO_VERSION) --target $(HOST_TARGET)`) and
  `dist-check` (= `dist-build` + `release-check.sh`).
  `HOST_TARGET` defaults to `rustc -vV`'s host triple so a stock
  developer Linux box runs the full pipeline on the gnu fallback;
  CI passes the musl/darwin/msvc targets explicitly.
- `packaging/README.md` (NEW) documents the toolchain story
  (`cargo-dist`, `cargo-zigbuild`, `zig` via `pipx ziglang`,
  rustup target list) plus the relationship to `release-plz`.
- `docs/src/contributing/release.md` (NEW) — the public-facing
  page on the cargo-dist ↔ release-plz handshake, `nexo version`
  semantics, how to add a new bin or new target. Registered in
  `docs/src/SUMMARY.md`.
- `CHANGELOG.md` root crate gets the Phase 27.1 entry under
  `## [Unreleased] / ### Added`. release-plz keeps owning
  per-crate `CHANGELOG.md` regeneration.

Deferred (now in `FOLLOWUPS.md`):
- Local musl validation requires `zig` + `cargo-zigbuild`
  versions that interop. zig 0.16.0 (current upstream pipx) is
  incompatible with cargo-zigbuild 0.22.x — full musl validation
  is CI-only until upstream catches up.
- macOS / Windows local validation needs the respective SDKs;
  Phase 27.2 CI is the right place to gate.
- `NEXO_BUILD_CHANNEL` injection from the release workflow
  (`apt-musl`, `brew-arm64`, etc.) lands when 27.2 wires the
  GH Actions matrix.

Done when (revised): `make dist-check` exit 0 on a stock
developer Linux box. The host-target tarball is built and
validated end-to-end; the rest of the matrix is a Phase 27.2
deliverable.

#### 27.2 — GitHub Actions release workflow   ✅

Scope reduced to **Linux + Termux only**. Apple
(`x86_64`/`aarch64-apple-darwin`) and Windows
(`x86_64-pc-windows-msvc`) targets dropped; Phase 27.6 (Homebrew)
parked. Re-enable: add the targets back to `dist-workspace.toml`,
restore matrix entries in `release.yml`, revive `packaging/homebrew/`.

Shipped:
- `.github/workflows/release.yml` rewritten end-to-end. Triggers
  on `push.tags: ["nexo-rs-v*"]` (matches release-plz's
  `git_tag_name`) and on `workflow_dispatch` with a `tag` input.
  `name: release` preserved so the `workflow_run` chain in
  `sign-artifacts.yml` (Phase 27.3) and `sbom.yml` (Phase 27.9)
  keeps firing without changes.
- 5 jobs: `validate-tag` (regex
  `^nexo-rs-v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$` + verifies
  the GH release exists), `build-musl` matrix x2 (x86_64 +
  aarch64, ubuntu-latest), `build-termux` (aarch64-linux-android,
  ubuntu-latest), `publish` (downloads artifacts +
  `gh release upload --clobber`), `smoke-test` (extracts host
  musl tarball + verifies short `--version` + provenance stamps).
- Toolchain pins: zig `0.13.0` via
  `goto-bus-stop/setup-zig@v2`, cargo-zigbuild `0.22.3`,
  cargo-dist `0.31.0`. Cache via `Swatinem/rust-cache@v2` keyed
  on `release-${target}-${hash(Cargo.lock)}`.
- `NEXO_BUILD_CHANNEL` injection per runner:
  `tarball-x86_64-unknown-linux-musl`,
  `tarball-aarch64-unknown-linux-musl`, `termux-aarch64`. Closes
  the Phase 27.1 deferral on build-channel provenance.
- Concurrency: `group: release-${{ github.ref_name }}`,
  `cancel-in-progress: false`. Re-runs of the same tag serialize;
  uploads never aborted mid-flight.
- `fail-fast: false` on the musl matrix.
- Permissions: `contents: write` only.
- `dist-workspace.toml` `targets` reduced to 2 musl entries;
  `installers = ["shell"]` (no PowerShell — no Windows target).
- `scripts/release-check.sh`: `EXPECTED_TARBALLS` reduced to 2
  musl; new Termux `.deb` glob check (`nexo-rs_*_aarch64.deb`)
  validates sha256 sidecar.
- `Makefile`: `HOST_TARGET ?= x86_64-unknown-linux-musl` (no more
  gnu host-fallback).
- `packaging/termux/build.sh`: emits `<deb>.sha256` sidecar at
  the end so `gh release upload` ships it.
- `packaging/README.md` rewritten — toolchain matrix, pinned
  versions of zig + cargo-zigbuild, drop mac/windows sections.
- `docs/src/contributing/release.md`: "automatic vs manual" table
  reflects Phase 27.2 ownership boundaries.

Deferred (in `FOLLOWUPS.md`):
- Termux runtime smoke (need Android emulator or device; ubuntu
  runner can't run bionic libc).
- Smoke-test auto-rollback (deletes assets on failure).
- `dist generate` vs hand-rolled `release.yml` drift watch.
- Apple + Windows targets revival.

Done when (revised): tag `nexo-rs-v<version>` push produces a GH
release with the 2 musl tarballs + Termux `.deb` + sha256 sidecars
in <15 min, downstream `sign-artifacts.yml` + `sbom.yml` triggered
automatically. Live validation requires the next release-plz PR
merge to produce the next `nexo-rs-v<version>` tag; the workflow
dispatch input is the manual fallback.

#### 27.3 — Cosign / sigstore signing   🔄

Image signing wired into the docker workflow (active now); binary
artifact signing wired as a standalone workflow (active once Phase
27.2 release.yml uploads assets); long-lived Homebrew bottle-key
deferred to a follow-up.

Shipped:
- `.github/workflows/docker.yml` extended with a
  `sigstore/cosign-installer@v3` step + `cosign sign --yes
  <tag>@<digest>` for every tag the workflow pushes. Keyless OIDC
  via GitHub Actions identity (`https://token.actions.github
  usercontent.com`) — no key material to manage. Each tag
  verifies against the same content digest. `permissions:
  id-token: write` already in place from Phase 27.5.
- `.github/workflows/sign-artifacts.yml` (NEW) — `workflow_run`
  triggered after the main release workflow completes a `v*`
  build. Downloads every `*.deb` / `*.rpm` / `*.tar.gz` /
  `nexo-*` asset from the release, runs `cosign sign-blob` with
  `--bundle` + `--output-signature` + `--output-certificate`,
  uploads the resulting `<asset>.sig`, `<asset>.bundle`,
  `<asset>.pem` back to the same release with `gh release
  upload --clobber`. `workflow_dispatch` input lets operators
  re-sign an old release on demand.
- `docs/src/getting-started/verify.md` (NEW) — one-page operator
  guide: why keyless (no long-lived key), how to install Cosign
  per OS, how to verify a Docker image (`cosign verify
  ghcr.io/...`), how to verify a downloaded asset
  (`cosign verify-blob --bundle`), CI-side verification snippet
  for deploy pipelines, how to inspect the Rekor transparency
  log, common failure modes, and what's still out of scope (PGP
  keys for apt/yum repos — Phase 27.4 follow-up; bottle-signing
  for Homebrew — Phase 27.6 follow-up).
- `docs/src/SUMMARY.md` registers the new page.

Deferred:
- A long-lived Cosign key for the Homebrew tap so `brew install`
  can validate without the OIDC chain (sigstore-go isn't in
  Homebrew's tooling yet). Tracked under 27.6.
- Auto-attestation that signs SBOMs + provenance separately from
  the image (today they ride together via
  `provenance: true, sbom: true` in the image build). Phase 27.9
  delivers separate attestation files.

Done when (revised): an operator can `cosign verify
ghcr.io/lordmacu/nexo-rs:<tag>` AND `cosign verify-blob
--bundle <asset>.bundle <asset>` and both pass against the
public Rekor log. Image side passes now; blob side activates
when 27.2 lands the release workflow.

#### 27.4 — Debian + RPM packages   ✅

**Tier 1** (downloadable `.deb` / `.rpm` as GH release assets) and
**Tier 3** (CI install-test matrix) shipped.
**Tier 2** (signed apt/yum repos in GH Pages) split out as
`27.4.b` — see below.

Shipped:
- `release.yml` gains 4 jobs: `build-debian` matrix x2 (`amd64` +
  `arm64`) + `build-rpm` matrix x2 (`x86_64` + `aarch64`). Both
  reuse the musl static binary already built by `build-musl`
  (cross-job artifact passing via `actions/download-artifact@v4`
  with `name: dist-${target}`); zero recompile.
- `release.yml` gains 2 install-test matrix jobs:
  `install-test-deb` on `debian:12` + `ubuntu:24.04` +
  `ubuntu:22.04` (`apt install ./nexo-rs_*_amd64.deb` + `nexo
  --version` regex match + `nexo --help`); `install-test-rpm` on
  `fedora:40` + `rockylinux:9` (`dnf install
  ./nexo-rs-*-x86_64.rpm` + same smoke). `fail-fast: false` per
  matrix; container `--user root`. Skip systemd boot test
  (containers without pid-1 systemd).
- `publish.needs:` extended to wait for the new build jobs;
  `download-artifact` already uses `pattern: dist-*` so the new
  `dist-deb-*` / `dist-rpm-*` artifacts are picked up
  automatically.
- `packaging/debian/build.sh` control file cleaned up:
  `Pre-Depends: adduser` (preinst runs before Depends are
  resolved), `Depends: ca-certificates` only (musl-static binary
  bundles libsqlite + libssl in), `Recommends:` ampliated to
  `nats-server, git, ffmpeg, tesseract-ocr, cloudflared, yt-dlp,
  python3`. `VERSION` extraction switched from a broken greedy
  awk to `grep -m1 '^version' | cut -d'"' -f2` (same fix applied
  to `packaging/rpm/build.sh` and `packaging/termux/build.sh`).
- `packaging/rpm/nexo-rs.spec` mirrors the cleanup: drop
  `Requires: sqlite-libs` and `Requires: openssl-libs`, keep
  `Requires: ca-certificates`, append `Recommends: cloudflared,
  yt-dlp, python3`. `packaging/rpm/build.sh::cp` of the systemd
  unit fixed to read from `packaging/debian/` (single source of
  truth).
- `docs/src/getting-started/install-deb.md` (NEW) — quick install
  + verify (sha256 + cosign) + start service + Recommends +
  uninstall + 27.4.b deferral note. Mirror at
  `docs/src/getting-started/install-rpm.md` (NEW). Both
  registered in `docs/src/SUMMARY.md`.
- `docs/src/contributing/release.md` "automatic vs manual" table
  expanded with the 4 new release.yml ownerships + 27.4.b
  deferred rows.

Deferred (to `27.4.b` below):
- Signed apt repo at `https://lordmacu.github.io/nexo-rs/apt/`
  with a clearsigned `InRelease` + GPG keyring.
- Signed yum/dnf repo at `.../yum/` with `RPM-GPG-KEY-nexo`.
- GitHub Pages publish job that mirrors release assets into the
  repo layout.
- One-line bootstrap installer (`curl ... | sh`) that wires the
  repo + key.

Deferred (general):
- arm64 docker install-test via qemu (~3 min overhead per image)
  — backlog until demand.
- systemd boot smoke (needs systemd-pid-1 container or VM).
- `NEXO_BUILD_CHANNEL` drift: the `.deb`/`.rpm` ship the binary
  built for the musl tarball, so `nexo --version --verbose`
  reports `channel: tarball-x86_64-unknown-linux-musl` even when
  installed via `dnf` / `apt`. Acceptable.

Done when (revised): tag `nexo-rs-v<version>` push produces 2
`.deb` + 2 `.rpm` + sha256 sidecars on the GH release; the
install-test matrix passes on 5 docker images.

#### 27.4.b — Signed apt/yum repos in GH Pages   ⬜

GPG key generation + management, repo metadata generation
(`apt-ftparchive` + `createrepo_c`), GH Pages publish job, and
the `nexo-rs.repo` / `apt sources.list` snippets that turn
`apt install nexo-rs` (or `dnf install nexo-rs`) into a one-liner
with auto-upgrades. Cosign keyless (Phase 27.3) covers
per-asset integrity but does NOT satisfy apt/yum trust chains —
GPG is a separate signing system serving distinct verification.

Done when an operator can drop a `sources.list` line + import the
public key once, then run `apt install nexo-rs` and `apt upgrade`
with package-manager-native UX.

#### 27.5 — Docker image at GHCR   ✅

- `Dockerfile` updated: builds the renamed `nexo` bin (was `agent`),
  uses `dumb-init` as PID 1 with `nexo` as exec target, OCI labels
  for `image.source` / `description` / `licenses`.
- `.github/workflows/docker.yml` — buildx multi-arch
  (`linux/amd64` + `linux/arm64`), `docker/metadata-action` tag
  set: `:latest` (default branch), `:v0.1.1`, `:v0.1`, `:v0`,
  `:edge`, `:main-<sha>`. Triggers on push to `main`, on `v*` tags,
  and on `workflow_dispatch`. Cache-from/to `type=gha` cuts ~10 min
  off cold builds. Provenance + SBOM attestations on by default.
- Auto-push to `ghcr.io/lordmacu/nexo-rs` with `GITHUB_TOKEN`
  (no extra secret required).
- `docker-compose.yml` updated: service renamed `agent` → `nexo`,
  `image:` field added pinning the GHCR pull so `compose up` works
  without `compose build`.
- Docs: `docs/src/ops/docker.md` documents the GHCR pull pattern,
  tag scheme, and how to verify provenance / SBOM with
  `docker buildx imagetools inspect`.

Deferred to a follow-up: distroless / musl-static variants
(`Dockerfile.release`, `Dockerfile.alpine`), `linux/arm/v7`
target. The current Debian-slim image is ~250 MB unpacked but
covers the runtime deps the browser plugin needs (Chrome on
amd64, Chromium on arm64) — going distroless requires removing
those, which is its own design decision.

#### 27.6 — Homebrew tap   🔄

Formula source-of-truth shipped in main repo; tap-repo mirror +
bottles deferred (block on Phase 27.2 release workflow).

Shipped:
- `packaging/homebrew/nexo-rs.rb` — formula building from source
  via `cargo install … --bin nexo`. Pinned `url` + `version` +
  `sha256` (placeholder sha256 — workflow rewrites on tag).
  `head` URL points at `main` so adventurous users can
  `brew install --HEAD nexo-rs`. License declared as
  `any_of: ["MIT", "Apache-2.0"]`. Build deps: `rust`,
  `pkg-config`. Runtime deps: `openssl@3`, `sqlite`. Caveats
  block lists optional channel-plugin tools (`ffmpeg`,
  `tesseract`, `yt-dlp`, `--cask google-chrome`). `test do`
  block verifies `nexo --version` matches and `nexo --help`
  surfaces the `setup` subcommand.
- `packaging/homebrew/README.md` — install one-liner (`brew tap
  lordmacu/nexo && brew install nexo-rs`), local audit recipe
  (`brew audit --strict --online`), explanation of how the
  release workflow keeps the tap repo in sync, and the deferred
  bottle plan (arm64_sequoia / arm64_sonoma / arm64_ventura /
  monterey).

Deferred:
- The actual tap repo at `https://github.com/lordmacu/homebrew-nexo`
  — created on demand when 27.2 release workflow opens its first
  bump PR.
- Bottles (pre-built binaries per macOS version) — needs a macOS
  CI runner + release workflow uploading `*.bottle.tar.gz` to
  the GitHub release. Today `brew install nexo-rs` source-builds
  in ~2-3 min on Apple silicon.
- Auto-PR job that bumps `version` + `url` + `sha256` on every
  `v*` tag — Phase 27.2 deliverable.

Done when (revised): `brew tap lordmacu/nexo && brew install
nexo-rs` works on a fresh macOS install AND the install pulls a
bottle (no source compile). Source-build path done now;
bottle / auto-PR halves block on 27.2.

#### 27.7 — Nix flake   ✅

- `flake.nix` at repo root: `packages.default` + `packages.nexo-rs`
  build from source via `rustPlatform.buildRustPackage`, MSRV
  pinned to 1.80 via `rust-overlay` so the flake stays in lockstep
  with `[workspace.package].rust-version`. `apps.default` exposes
  the `nexo` bin for `nix run`. `devShells.default` ships
  rustc + clippy + rustfmt + cargo-edit/watch/nextest/deny + mdbook
  + mdbook-mermaid for contributors.
- `docs/src/getting-started/install-nix.md` documents the install
  one-liner, the dev-shell command, the runtime tools the flake
  *doesn't* install (chrome / cloudflared / ffmpeg / tesseract /
  yt-dlp — those are system-level), pin-to-release pattern, and
  enabling `experimental-features = nix-command flakes`.
- `docs/src/SUMMARY.md` registers the new install page.

`cachix` binary cache deferred — first push to `main` rebuilds
from source on the user side. When the cache lands, `nix run`
becomes instant.

Done when `nix run github:lordmacu/nexo-rs -- --help` builds and
prints help (currently ~3-5 min cold; sub-30s once cachix is on).

#### 27.8 — Termux package recipe   🔄

Recipe + docs shipped; release-workflow upload + pkg-index host
deferred (block on Phase 27.2).

Shipped:
- `packaging/termux/build.sh` builds
  `dist/nexo-rs_<version>_aarch64.deb` either by cross-compiling
  via `cargo-zigbuild` (host path) or by accepting a pre-built
  binary (`--binary <path>` for native Termux builds). Reads
  version + description from `Cargo.toml` — no drift. Falls back
  to `fakeroot + ar` on hosts without `dpkg-deb`.
- The deb stages under `data/data/com.termux/files/usr/` (Termux
  `$PREFIX`), drops `nexo` in `bin/`, ships LICENSE-APACHE +
  LICENSE-MIT + README.md under `share/`, and ships a `postinst`
  that scaffolds `~/.nexo/{data,secret}` on first install +
  prints next steps.
- `Depends:` pulls hard runtime deps (`libsqlite`, `openssl`,
  `ca-certificates`); `Recommends:` covers optional skill deps
  (`git`, `ffmpeg`, `tesseract`, `python`, `yt-dlp`,
  `dumb-init`) so a minimal install still boots.
- `packaging/termux/README.md` documents local cross-compile
  path, native-on-phone build path, install command, why Termux
  needs its own deb (bionic libc + non-standard `$PREFIX`
  layout), and the Termux-specific limitations (no browser
  plugin, no `cloudflared`).
- `docs/src/getting-started/install-termux.md` adds a "Quickest
  path — pre-built .deb" section above the existing
  source-build walkthrough.

Deferred:
- Release workflow upload of the .deb as a GitHub release
  artifact — needs Phase 27.2 (cargo-dist + GH Actions release
  workflow) to land first.
- Termux pkg index hosted at
  `https://lordmacu.github.io/nexo-rs/termux/` with `Packages`
  + `Release` files so users can add it as a `pkg` repo. Today
  the .deb is downloaded directly from the GitHub release.

Done when on a fresh Termux a user runs
`pkg install -y nexo-rs` (after adding the repo) and the
binary lands in `$PREFIX/bin/`.

#### 27.9 — SBOM + reproducibility   🔄

SBOM workflow + reproducibility docs shipped; SLSA-verifier integration
+ pinned-Debian-package Docker layer deferred.

Shipped:
- `.github/workflows/sbom.yml` (NEW) — `workflow_run` triggered
  after release. Generates two SBOMs:
  - `sbom-cyclonedx.json` via `cargo cyclonedx --all` (cargo dep
    tree with versions + hashes).
  - `sbom-spdx.json` via `syft .` (full filesystem scan; catches
    bundled binaries / generated assets that cargo doesn't track).
  - Both signed with Cosign keyless OIDC (`*.bundle`) reusing
    the Phase 27.3 chain.
  - Attached to the release with `gh release upload --clobber`.
- Docker image SBOM continues to ride via `provenance: true,
  sbom: true` in `docker.yml` (Phase 27.5).
- `docs/src/getting-started/reproducibility.md` (NEW) — operator
  guide. How to read the CycloneDX + SPDX SBOMs (`jq` recipes,
  `cargo audit`, `grype` against the SBOM file). The
  reproducible-build claim spelled out: pinned Rust toolchain
  (`rust-toolchain.toml`), pinned deps (`Cargo.lock` + `--locked`),
  pinned environment (`ubuntu-latest`), no `RUSTFLAGS` overrides.
  Local-reproduction recipe, common reasons hashes diverge
  (different glibc, different LLVM, local `~/.cargo/config.toml`),
  guaranteed-reproducible recipe via `docker run rust:1.80-bookworm`.
  Pre-emptive `slsa-verifier verify-artifact` snippet for when
  Phase 27.2 wires the SLSA attestation.
- `docs/src/SUMMARY.md` registers the new page.

Deferred:
- SLSA Level 2 attestation produced by
  `actions/attest-build-provenance` per binary asset (snippet
  documented; wires up when 27.2 release.yml lands).
- `slsa-verifier verify-artifact` smoke test in CI to catch
  attestation regressions.
- Pinned-version `apt-get install` in `Dockerfile` so the
  Debian-slim runtime layer is itself reproducible. Today
  `apt-get install` pulls whatever's latest. Tracked under
  Phase 34 hardening cross-link.

Done when (revised): `slsa-verifier verify-artifact` against a
release passes AND a third party rebuilds the binary in the
documented Docker container and gets the same sha256. SBOM half
done now; verifier wiring blocks on 27.2.

#### 27.10 — Install docs + first-run   🔄

Landing page + per-channel pages now in place; `--version` build
provenance + `self-update` deferred.

Shipped:
- `docs/src/getting-started/installation.md` rewritten as a
  "pick your channel" landing page with a matrix (Docker /
  Nix / Native / Termux / source), time-to-first-run, and
  bundled runtime-tools column. Stale `agent` bin refs swapped
  to `nexo` (post-rename in 4bccdc3); stale "18 crates, 4
  binaries" updated to "22 crates".
- Channel pages already shipped:
  - [Docker](../ops/docker.md) — Phase 27.5
  - [Nix flake](./install-nix.md) — Phase 27.7
  - [Native install](./install-native.md) — pre-existing
  - [Termux install](./install-termux.md) — pre-existing

Deferred:
- `nexo --version` printing the install-channel marker
  (`v0.1.1+brew-arm64`) so bug reports carry provenance —
  needs `cargo dist`-side metadata injection (Phase 27.1).
- `nexo self-update` GH-releases poller + prompt — needs
  `cargo dist` releases as the source of truth (Phase 27.1).
- `apt`, `yum`, `brew` channel-specific pages — wait for
  Phases 27.4 / 27.6 to actually ship those packages.

Done when (revised): the install landing page lists every
channel with copy-paste blocks AND the `nexo --version`
provenance line is wired. First half done now; second half
blocks on 27.1.

**Phase 27 done when** an operator can run any one of:
`brew install nexo-rs`, `apt install nexo-rs`,
`docker run ghcr.io/lordmacu/nexo-rs`, `pkg install nexo-rs`
on Termux, `nix run github:lordmacu/nexo-rs` — and the
artifact carries a signed SBOM verifiable end-to-end.

### Phase 28 — Production observability   🔄

Grafana dashboards bundled (active now). OTel propagation,
`/api/costs` admin endpoint, scrape-config tweaks deferred.

Shipped (28.1):
- `ops/grafana/nexo-overview.json` — single-screen executive
  view: tool throughput, LLM TTFT p50/p95/p99, web-search
  breaker opens, tool cache hit ratio.
- `ops/grafana/nexo-llm.json` — TTFT quantiles by provider,
  chunk emission breakdown, link-understanding fetch latency +
  outcomes + cache.
- `ops/grafana/nexo-tools.json` — tool latency p95/p99 by tool,
  calls × outcome stack, MCP sampling activity, web-search
  calls + latency by provider.
- `ops/grafana/README.md` — import via UI / API / Grafana
  provisioning, Prometheus scrape config snippet, full metric
  coverage table cross-referencing source crate + originating
  phase, dashboard editing round-trip protocol (strip `id`,
  bump `version` before commit).

Each panel uses a `${DS_PROMETHEUS}` datasource variable so the
operator binds to whichever Prometheus the deployment uses.
Refresh defaults to 30s. Tags `nexo` + dashboard role so a
folder filter shows them together.

Deferred:
- **OpenTelemetry traces** — W3C context propagation across NATS
  hops (`event.tracing` field carries traceparent), OTLP
  exporter behind `runtime.observability.otel.endpoint` so
  traces ship to Jaeger / Tempo / Honeycomb without a code
  change. Larger surface — own sub-phase 28.2.
- **Cost dashboard** — per-agent / per-binding / per-session
  token aggregation table (rolling 24h + 30d) exposed via
  `nexo costs` CLI and a `/api/costs` admin endpoint. Series
  not yet emitted; lands when the cost-accumulator (Phase 45)
  ships.
- **TaskFlow status panel** — flow counts by state. Needs the
  `nexo_taskflow_status` gauge to be added first.
- **DLQ depth panel + alert** — `nexo_broker_dlq_depth` gauge
  not yet emitted.

Done when (revised): an operator drops the bundled dashboards
into Grafana, sees them populate from the live `/metrics`
endpoint without writing a single PromQL query, AND traces
flow end-to-end via OTel. Dashboards done now; OTel + cost
dashboard land in 28.2 + 28.3.

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
new `Gateway` server runs alongside the agent runtime.

##### 30.1.1 — Crate scaffold + WebSocket listener

- New crate `crates/gateway/` with `tokio-tungstenite` server
  bound by default on `127.0.0.1:18789`.
- TLS via `rustls`: self-signed cert at first launch under
  `data/gateway/cert.pem`, regenerable. Operators bring their
  own cert when serving on a real hostname.
- Bind modes config in `gateway.yaml`:
  `bind: loopback | lan | tailscale | tunnel`.
- `gateway.tls: { mode: self_signed | provided | tunnel }`.

##### 30.1.2 — JSON-RPC 2.1 frame format

- `RequestFrame { id, method, params }` /
  `ResponseFrame { id, result | error }` /
  `EventFrame { stream_id, kind, payload }`.
- `serde`-derived types under `crates/gateway/src/wire.rs`.
- A `gateway-schema.json` (JSON Schema Draft 2020-12) committed
  to `docs/schema/` so the Flutter side generates Dart types
  from the same source of truth.
- Schema regression test: parse every recorded frame against the
  schema in CI.

##### 30.1.3 — Methods (initial set)

Implement in this order, each as its own commit + integration
test:

1. `Connect` (handshake, returns server info + capabilities)
2. `Wake` (heartbeat, returns server time)
3. `Agents.list`
4. `Agents.files.{list, get}`
5. `Send` (publish to a per-agent inbox)
6. `Poll` (subscribe to events for an agent)
7. `Agents.create / update / delete`
8. `Agents.files.set`
9. `MessageAction` (reply, react, mark-read)
10. `Commands.list`
11. `NodePair.request` (deferred to 30.2)

##### 30.1.4 — Capability-scoped tokens

- `crates/gateway/src/capabilities.rs` — bitmask + named scopes
  (`agents.read`, `agents.write`, `chat.send`, `files.read`,
  `files.write`, `admin.full`).
- Token format: opaque random 32-byte string; HMAC-SHA256 of the
  token + per-launch secret stored in `pairing_tokens` table.
- Handshake validates token → loads scope set → attached to
  every dispatched method.
- Method dispatcher refuses calls outside the token's scope with
  JSON-RPC `-32601 Method not allowed`.

##### 30.1.5 — Bind-mode resolvers

- `loopback` — bind to 127.0.0.1, document SSH-tunnel pattern.
- `lan` — bind to the resolved LAN interface, refuses if the
  host has only loopback.
- `tailscale` — runs `tailscale status --json`, picks the
  tailnet hostname, binds the Tailscale-IP interface.
- `tunnel` — reuses `crates/tunnel/` (cloudflared) on a separate
  subdomain so the admin UI and the gateway don't share a URL.

#### 30.2 — Bootstrap pairing

Pairing flow as in OpenClaw `src/pairing/`.

##### 30.2.1 — Setup-code generator + CLI

- `agent pair start [--label "<text>"] [--scope <scope_set>]`
  prints:
  - 6-digit numeric setup code (entropy: 20 bits, throwaway).
  - Gateway URL appropriate for the active bind mode.
  - QR code rendered to terminal containing
    `nexo://pair?host=…&code=…&label=…`.
- Code expires after 10 minutes; daemon stores
  `pending_pairings` row keyed by code.

##### 30.2.2 — `NodePair.request` handler

- Device sends
  `{ code, device_info: { name, platform, os_version, app_version } }`.
- Daemon validates code (single-use), generates a
  `bootstrapToken`, persists in `pairing_tokens` with
  `device_id`, `label`, `scopes`, `created_at`, `last_seen`.
- Returns `{ token, server_info, scopes }`.
- Code consumed even on failed scope check so a leaked code can
  only be tried once.

##### 30.2.3 — `pairing-store` schema

- New SQLite at `data/gateway.db` with tables:
  - `pairing_tokens (device_id PK, token_hash, label, scopes,
    created_at, last_seen, revoked_at)`
  - `allow_from_store (device_id, channel, instance,
    allowed BOOL)` — per-device per-channel grant.
  - `pending_pairings (code PK, scope, label, created_at,
    expires_at)`.
- Migration via Phase 36's `agent migrate`.

##### 30.2.4 — Token CLI: list / revoke / show

- `agent pair list` — lists devices, last-seen, scope summary.
- `agent pair revoke <device_id>` — sets `revoked_at`,
  in-flight WS connections drop.
- `agent pair show <device_id>` — JSON dump for debugging.

##### 30.2.5 — Threat-model doc

- `docs/src/architecture/pairing-threat-model.md` covering:
  - Setup-code lifetime + replay protection.
  - Token storage on device (Keychain / Keystore).
  - Network shape under each bind mode (who can MITM).
  - Revocation latency.
  - Compromised-device playbook.

#### 30.3 — Flutter shell + shared protocol client

##### 30.3.1 — `apps/flutter/` scaffold

- `flutter create apps/flutter --platforms=android,ios,macos,web,linux,windows`.
- `pubspec.yaml` with pinned versions for `riverpod`, `drift`,
  `flutter_secure_storage`, `web_socket_channel`, `local_auth`,
  `mobile_scanner` (QR), `system_tray` (desktop), `intl` (i18n).
- CI: `flutter analyze`, `flutter test`, build matrices on GH.

##### 30.3.2 — `nexo_protocol` Dart package

- `apps/flutter/packages/nexo_protocol/` — generated from
  `docs/schema/gateway-schema.json` via
  `quicktype` or `json_serializable`.
- A `make schema` recipe regenerates Rust + Dart from the same
  source.
- Round-trip test: every fixture frame deserializes identically
  in Rust and Dart.

##### 30.3.3 — Transport layer

- `lib/transport/gateway_client.dart` — WS reconnect with
  exponential backoff, heartbeat (`Wake` every 30s),
  out-of-order ack handling.
- Pluggable backend: real WS for prod, in-memory fake for
  widget tests.
- Telemetry: connection state stream Riverpod-exposed.

##### 30.3.4 — Core screens

Build these in order, each ships behind a feature flag:

1. `pairing/` — QR scan + manual code, calls
   `NodePair.request`, persists token.
2. `chat/` — per-agent chat surface with send + media + voice.
3. `agents/` — list, view, edit SOUL/MEMORY (calls
   `Agents.files.{get,set}`).
4. `events/` — live timeline.
5. `approvals/` — biometric-gated action queue.
6. `settings/` — gateway URL, devices, notifications, theme.

##### 30.3.5 — Local persistence

- `drift` schema: `messages`, `events`, `agents_cache`.
- Token in `flutter_secure_storage` (Keychain / Keystore).
- Background sync: when the app comes to foreground, replays
  events since `last_seen` via `Poll`.

#### 30.4 — Platform-specific bridges

##### 30.4.1 — iOS bridges

- `ShareExtension` (Swift) — accepts shared text/image/url,
  pushes via the gateway as a `Send`.
- `NotificationServiceExtension` (Swift) — decrypts and
  renders push payload bodies.
- `WatchKit` companion target (Swift) — recent-events glance.
- iMessage bridge (privileged, opt-in): polls `chat.db` if the
  user grants Full Disk Access, posts new messages.
- Push registration via APNs token; daemon hands it to a Phase
  41-compatible push server.

##### 30.4.2 — Android bridges

- Foreground `Service` (Kotlin) maintaining the WS under doze.
- `ShareTarget` intent filter in `AndroidManifest.xml`.
- Direct-Reply on `NotificationCompat` so the user can reply
  from the notification shade.
- SMS bridge (opt-in, READ_SMS / SEND_SMS perms): polls inbox,
  posts to the gateway as a `plugin.inbound.sms` event.
- FCM token registration mirroring iOS APNs.

##### 30.4.3 — macOS tray

- `system_tray` Flutter package shows status + last 10 events.
- `URLSchemeHandler` registers `nexo://` so QR-scan-to-pair
  works from Safari.
- Auto-launch on login via `LaunchAgents` plist.

##### 30.4.4 — Linux + Windows desktop

- No special bridges; Flutter desktop bundle ships the chat
  shell.
- Linux: `.desktop` entry + AppImage.
- Windows: MSIX with `nexo://` URL handler.

##### 30.4.5 — Web target (PWA)

- Flutter web build deployed to
  `https://lordmacu.github.io/nexo-rs/app/`.
- `manifest.json` for installable PWA.
- Service worker for offline cache + Web Push subscription.
- WebSocket fallback to long-poll when WS is blocked
  (corporate proxies).

#### 30.5 — Distribution

##### 30.5.1 — iOS TestFlight + App Store

- `apps/flutter/ios/fastlane/` with the TestFlight upload lane.
- Privacy manifest + App Store review notes covering the share
  extension and the iMessage opt-in.
- Internal beta to a small group; public release after the
  privacy review clears.

##### 30.5.2 — Android Play + APK sideload

- `fastlane supply` lane to Play Internal Test → Closed Test
  → Production.
- AAB upload + standalone APK at the GH release for sideload.
- Play Console listing copy + screenshots automated.

##### 30.5.3 — macOS DMG (notarized)

- Fastlane lane: build, codesign with Developer ID, notarize,
  staple, build DMG via `create-dmg`.
- Sparkle update feed pointed at the GH releases (Phase 27).

##### 30.5.4 — Windows MSIX + Linux AppImage / Flatpak

- MSIX packaged with self-signed cert (sideload path) and
  later a real CA cert.
- Linux: AppImage primary, Flatpak via flathub once stable.

##### 30.5.5 — Web target deploy

- Same `.github/workflows/docs.yml` (Phase 27 follow-up)
  builds Flutter web on tag and deploys to GH Pages alongside
  the mdBook.
- Subdomain option: `app.nexo-rs.dev` if the operator wants a
  cleaner URL.

**Phase 30 done when** all 5 sub-phases tick green: gateway
running, devices paired, Flutter shell shipping core screens,
each platform-specific bridge tested by hand on a real device,
and at least the iOS TestFlight + Play Internal Test channels
are accepting testers.

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

### Phase 35 — Performance + benchmarks   🔄

Bench scaffolding shipped on `nexo-resilience`. Other hot-path
crates + load-test rig + memory profiling deferred.

Shipped (35.1):
- `crates/resilience/benches/circuit_breaker.rs` — criterion 0.5
  benchmark suite covering the breaker hot paths:
  - `allow` against a closed breaker (closed-state allow is the
    most common call by orders of magnitude)
  - `allow` against an open breaker (validates the early-exit
    path stays cheap)
  - `on_success` and `on_failure` transitions
  - 8-task concurrent `allow` hammering for contention sniffing
  Run with `cargo bench -p nexo-resilience`; output lands in
  `target/criterion/`. Future regressions in any of those four
  paths surface immediately.
- `crates/resilience/Cargo.toml` adds `criterion = "0.5"` as
  dev-dep + `[[bench]] name = "circuit_breaker"` registration.

Shipped (35.2):
- `crates/broker/benches/topic_matches.rs` — covers the hottest
  function in `LocalBroker::publish` (every published event
  scans `topic_matches` against every active subscription
  pattern). Three groups: exact-match, wildcard-match (`*` and
  `>`), and a "wildcard storm" that evaluates 50 patterns
  against one subject, approximating a 15-agent deployment.
- `crates/broker/benches/local_publish.rs` — end-to-end
  publish path: lock-free `DashMap` scan, `try_send` per
  matching subscriber, slow-consumer drop-counter increments.
  Four groups: zero-subscriber publish (worst-case miss),
  one-subscriber exact, 10-subscriber wildcard fan-out,
  50-subscriber realistic mix. Uses `Throughput::Elements` so
  criterion reports msgs/sec.
- `crates/broker/Cargo.toml` adds `criterion = "0.5"`
  + two `[[bench]]` registrations.

Shipped (35.3):
- `crates/llm/benches/sse_parsers.rs` — covers the three
  streaming SSE parsers (`parse_openai_sse`,
  `parse_anthropic_sse`, `parse_gemini_sse`) with realistic
  fixtures: 50 text-delta chunks each (typical short answer).
  OpenAI fixture also covers OpenAI-compat providers (minimax,
  deepseek, mistral.rs, ollama, vllm, llama.cpp, LM Studio).
  Anthropic fixture exercises the explicit `event:` framing.
  Gemini fixture covers the JSON-per-data-line shape. All three
  use `Throughput::Elements(N)` so criterion reports chunks/sec.
- `crates/llm/Cargo.toml` adds `criterion = "0.5"` as dev-dep
  + `[[bench]] name = "sse_parsers"` registration.

Shipped (35.4):
- `crates/taskflow/benches/tick.rs` — `WaitEngine::tick`
  bench at 10 / 100 / 1 000 active waiting flows, all with
  future-timer waits (no due flows so the path measured is
  purely "scan the store, decide nothing matures yet"). Uses
  in-memory SQLite for hermetic, sub-100ms setup per case.
  Throughput reported in flows/sec scanned. Sub-millisecond is
  the target at single-host scale; this bench traps regressions
  on the SQL query plan or the in-memory cursor logic.
- `crates/taskflow/Cargo.toml` adds `criterion = "0.5"` as
  dev-dep + `[[bench]] name = "tick"` registration.

Shipped (35.6):
- `.github/workflows/bench.yml` (NEW) — matrix over
  `nexo-resilience` / `nexo-broker` / `nexo-llm` /
  `nexo-taskflow`. Triggers: PRs touching `crates/**` /
  `Cargo.lock` / `Cargo.toml`, weekly Sunday 04:00 UTC main
  run, manual workflow_dispatch. Each run saves a per-PR or
  `main` baseline via `--save-baseline`. Cargo cache shared
  per-crate so weekly runs build on the previous baseline
  instead of starting cold. Artifacts retained 30 days.
- `docs/src/ops/benchmarks.md` (NEW) — operator + contributor
  reference. Quick-run cheatsheet, full coverage matrix
  cross-referencing crate / bench / hot-path / target latency,
  pattern for adding a new bench, CI integration semantics
  (when each baseline saves where), known limitations of GH
  Actions runners (~5-10% noise on the shared tier), criterion
  output reading guide.
- `docs/src/SUMMARY.md` registers the new page under
  Operations.

Today the CI job is **informational** — a regression doesn't
fail the PR. Once ~10 `main` baselines accrue per crate, the
workflow gates on `>10% regression` per group (35.6
done-criterion).

Deferred:
- **35.5** Transcripts FTS search bench, redaction pipeline
  bench. Memory profiling via `dhat` snapshots at idle vs
  load.
- **35.6 final** PR-comment bot that posts criterion deltas
  inline (needs the per-crate noise-floor measurement first).
- **35.7** PGO release builds.
- **35.3** End-to-end load-test rig that spawns N inbound
  messages over the local broker and measures tail latency at
  varying agent counts.
- **35.4** Memory profiling via `dhat` snapshots at idle vs
  load, documented RSS at 1 / 10 / 100 sessions.
- **35.5** Comparison table vs OpenClaw on equivalent workloads
  (same prompt, same provider) for the "we're not just claiming
  faster" piece.
- **35.6** CI gate that fails a PR on >10% regression in any
  benched path, using `criterion-compare-action`.
- **35.7** PGO (`-Cprofile-use`) for release binary — gated on
  having a measured baseline first (35.1-35.5).

Done when (revised): `cargo bench -p nexo-resilience` produces
deterministic numbers in CI AND a comparable suite covers the
workspace's other hot paths AND the README claim "Rust faster"
points at a real number table. Scaffolding done; substance
follows when 35.2-35.5 land.

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

### Phase 36 — Backup, restore, migrations   🔄

Shell bridge + operator doc shipped. Runtime subcommands +
versioned migrations + encrypted output deferred to follow-ups.

Shipped (36.2 — `nexo-memory-snapshot` crate, agent memory snapshot/restore):
- New crate `crates/memory-snapshot/` — atomic point-in-time bundle of an agent's
  full memory: git memdir + 4 SQLite stores (long_term, vector, concepts,
  compactions) + extractor cursor + last dream-run row, packaged as
  `tar.zst` (or `.tar.zst.age`) with manifest seal + sibling whole-file
  SHA-256.
- Trait surface: `MemorySnapshotter` (snapshot/restore/list/diff/verify/
  delete/export). Default impl `LocalFsSnapshotter` with per-agent
  `tokio::sync::Mutex` lock map + `DashMap` keying.
- SQLite backup via `VACUUM INTO` (online, atomic, WAL-safe; zero extra
  deps on top of the workspace's `sqlx`).
- Git memdir captured directly as `.git/**` tar entries; `git_capture`
  reads HEAD via `git2`, drops a `pre-restore-<id>` tag before each
  restore so prior state is reachable from `git reflog`.
- Optional redaction layer (`DefaultRedactionPolicy`) covering
  Anthropic / OpenAI / AWS / GitHub / Slack / bearer / JWT shapes,
  applied to `memory_files/**` + `state/**` before sealing.
- Optional `age` encryption behind Cargo feature `snapshot-encryption`;
  manifest stays inside the encrypted body but the per-artifact hashes
  commit to it; sibling `.sha256` always covers the bytes on disk.
- Restore flow: verify → optional `auto-pre-snapshot` (label
  `auto:pre-restore-<orig>`) → atomic SQLite swap with `.pre-restore.bak`
  siblings → memdir replace with `<memdir>-pre-restore-<id>/` backup →
  state-provider replay. `--dry-run` builds the report without mutating.
- `RetentionWorker` background task: `keep_count` + `max_age_days`
  pruning that obeys the "never delete the last snapshot" floor, plus
  startup + per-tick orphan staging cleanup
  (`.staging-*` and `.restore-staging-*` left behind by SIGKILL).
- NATS event wiring: `nexo.memory.snapshot.<agent>.{created,restored,
  deleted,gc}` lifecycle subjects + `nexo.memory.mutated.<agent>` for
  per-write mutation events. `EventPublisher` trait + `NoopPublisher`
  default — broker bridge is plugged at boot.
- Per-tenant scope: `<state_root>/tenants/<tenant>/snapshots/<agent>/`
  with lexical path validators. Cross-tenant restore refused before any
  disk mutation.
- `MemorySnapshotConfig` lives in two parallel forms:
  `nexo_memory_snapshot::config::MemorySnapshotConfig` is the
  runtime API; `nexo_config::types::memory::SnapshotYamlConfig`
  (with sub-blocks `SnapshotEncryptionYamlConfig`,
  `SnapshotRetentionYamlConfig`, `SnapshotEventsYamlConfig`) is the
  wire shape. Same cycle-break pattern Phase 77.7 used for
  `SecretGuardYamlConfig`. Boot wire in `src/main.rs` reads
  `cfg.memory.snapshot` directly: `enabled` toggles the subsystem,
  `root` / `memdir_root` / `sqlite_root` override the on-disk
  layout, `lock_timeout_secs` flows into the builder,
  `retention.{keep_count,max_age_days,gc_interval_secs}` flows
  into `RetentionWorker`, `redact_secrets_default` flows into the
  per-agent `MemorySnapshotTool::with_redact_secrets_default`. 3
  YAML round-trip tests in `crates/config/tests/load_test.rs`
  guard the schema (default omission, full block, unknown-field
  rejection).
- `crates/setup/src/capabilities.rs::INVENTORY` registers
  `NEXO_MEMORY_RESTORE_ALLOW` (extension `memory-snapshot`, Critical,
  Boolean) so the doctor surfaces it and operators must opt in to
  enable `nexo memory restore`.
- Prometheus metrics: `nexo_memory_snapshot_total{agent,tenant,outcome}`,
  `_restore_total`, `_gc_total`, `_bytes_total`, `_duration_ms` histogram
  (8 buckets 50ms→60s) with 256-label cardinality cap. Pattern aligns
  with `nexo-llm`/`nexo-web-search`/`nexo-mcp` telemetry.
- 141 unit tests green (134 default + 7 with `snapshot-encryption`),
  plus 28 trait-suite tests under `nexo-driver-types`, 5 LLM-tool
  tests under `nexo-core`, 3 expose-tools regression guards, and 2
  end-to-end tests in
  `crates/memory-snapshot/tests/e2e_pre_dream_test.rs` that drive
  the `PreDreamSnapshotAdapter` against a real `LocalFsSnapshotter`
  through the full snapshot → verify → mutate → restore chain.
- Operator guide: `docs/src/ops/memory-snapshot.md` (registered in
  `SUMMARY.md`).
- `crates/driver-types/src/pre_dream_snapshot.rs` introduces a
  `PreDreamSnapshotHook` trait. `crates/dream/src/auto_dream.rs`
  gains `with_pre_dream_snapshot` + `with_pre_dream_tenant` builders
  and fires the hook between the dream-run audit insert and the
  fork dispatch. Failure logs `tracing::warn!` and the dream
  proceeds — operators who want a hard gate enforce it at the boot
  wire by omitting the hook.
- `crates/memory-snapshot/src/dream_adapter.rs` ships
  `PreDreamSnapshotAdapter`, the boot-time bridge that turns any
  `Arc<dyn MemorySnapshotter>` into a `PreDreamSnapshotHook` ready
  to plug into `AutoDreamRunner`.
- `crates/driver-types/src/memory_mutation.rs` introduces
  `MemoryMutationHook`, `MemoryMutationScope`, `MemoryMutationOp`.
- `crates/memory/src/long_term.rs::LongTermMemory` now carries an
  optional `Arc<dyn MemoryMutationHook>` (set via
  `with_mutation_hook`) and a tenant string (set via
  `with_tenant`). `remember_typed` fires `Insert`, `forget` fires
  `Delete` (after capturing the row's `agent_id` pre-DELETE so the
  event payload is correct). Best-effort: hook failure cannot
  poison the writer's transaction, and an absent hook short-circuits
  silently. 3 new integration tests in
  `crates/memory/tests/long_term_test.rs` cover happy path,
  no-hook tolerance, and missing-id non-event.
- Fire-site wiring of the remaining writers
  (`vector`/`concepts`/`compactions` SQLite stores, plus
  `crates/core/src/agent/workspace_git.rs` git memdir commits)
  follows the same pattern: optional field + builder + best-effort
  invocation post-transaction. Boot wire in `Mode::Run` requires a
  small init-order shuffle so the snapshotter (and its
  publisher-bridge) exists before `LongTermMemory::open_with_vector`
  is called — tracked separately so the surgery stays isolated.
- `MemoryMutationPublisher` in `dream_adapter.rs` bridges the
  driver-types hook to `EventPublisher`, so a single boot wire
  exposes every memory write on the
  `nexo.memory.mutated.<agent_id>` NATS subject.
- `admin-ui/PHASES.md::Phase A7` now lists the snapshot panel
  responsibilities (list + trigger + restore + verify + diff +
  retention status + lifecycle event tail).
- `crates/core/src/agent/memory_snapshot_tool.rs` ships the
  `memory_snapshot` LLM tool (write-only, deferred schema). Args
  `{label?, redact_secrets?}`; returns `{snapshot_id, bundle_path,
  bundle_size_bytes, bundle_sha256, created_at_ms,
  redactions_applied, encrypted}`. `created_by="tool"` on every
  call so audit can distinguish operator vs LLM-driven snapshots.
  Restore is intentionally not exposed — operator-only via the
  CLI to keep prompt-injection out of destructive rollback paths.
- `crates/core/src/plan_mode.rs::MUTATING_TOOLS` and
  `crates/config/src/types/mcp_exposable.rs::EXPOSABLE_TOOLS` both
  register `memory_snapshot`. `expose_tools_typo_regression_test`
  bumped accordingly so a future rename fails loud.
- `src/main.rs` ships the full `nexo memory <sub>` operator surface:
  `Mode::Memory(MemorySubcommand)` with seven verbs —
  `snapshot`, `restore`, `list`, `diff`, `export`, `delete`,
  `verify` — wired via `route_memory_subcommand` (cron/pair-style
  routing so kv-flag values do not shift positional arity). Each
  handler constructs a fresh `LocalFsSnapshotter` from
  `--state-root` (+ optional `--memdir-root` / `--sqlite-root`)
  and exits when the action completes. No daemon contact required.
  `verify` exits with code 2 when any integrity check fails;
  `restore` is gated on `NEXO_MEMORY_RESTORE_ALLOW=true` (capability
  inventory entry) and refuses without it; `delete` requires
  `--yes` so a typo cannot drop a bundle. End-to-end smoke
  validated against `/tmp` fixture: `snapshot` → bundle on disk,
  `list` shows it, `verify` reports clean, all exit codes match.
- `Cargo.toml` (root) gains feature pass-through
  `snapshot-encryption = ["nexo-memory-snapshot/snapshot-encryption"]`
  so operators can opt in encryption with one binary flag.

Boot wire (Mode::Run) — shipped in 36.2:
- `src/main.rs` builds a single `Arc<dyn MemorySnapshotter>` from
  `nexo_state_dir()` near the daemon's other shared globals
  (next to `flow_manager`). The same instance flows into every
  per-agent tool registry — operator CLI snapshots and LLM-tool
  snapshots land in the same per-agent dir.
- `RetentionWorker` spawned with the daemon's `dream_shutdown`
  cancellation token. Initial sweep + periodic sweep run with
  default config (keep=30, max_age=90d, gc_interval=1h). Orphan
  `.staging-*` and `.restore-staging-*` dirs left by SIGKILL are
  deleted on the first tick after boot.
- Per-agent tool registry registers `MemorySnapshotTool` alongside
  `MemoryHistoryTool` / `MemoryCheckpointTool`. Per-binding gating
  still flows through `EffectiveBindingPolicy::allowed_tools` +
  plan-mode (the tool sits in `MUTATING_TOOLS`).
- `nexo_dream::boot::BootDeps` extended with
  `pre_dream_snapshot: Option<Arc<dyn PreDreamSnapshotHook>>` +
  `pre_dream_tenant: String` so when the binary's `BootDeps`
  consumer lands, attaching the `PreDreamSnapshotAdapter` is one
  field assignment. `build_runner` already threads the hook
  through `with_pre_dream_snapshot` / `with_pre_dream_tenant`.
- LLM tool `memory_snapshot` (write-only) under `crates/core/src/agent/`,
  registered in `MUTATING_TOOLS` + `EXPOSABLE_TOOLS`.
- Fire-site wiring of `MemoryMutationHook` inside
  `crates/memory/{long_term,vector,concepts,compactions}.rs` and
  `crates/core/src/agent/workspace_git.rs` so every write actually
  drives the hook (the trait + adapter already exist).
- Migration of `MemorySnapshotConfig` into `nexo-config::types::memory`
  with schema bump v11 → v12.
- e2e test that exercises dream → auto-pre-snapshot → restore in a
  single in-process run.

Shipped (36.1):
- `scripts/nexo-backup.sh` — hot backup script. Uses
  `sqlite3 .backup` (online-backup mechanism, captures a
  consistent point-in-time image even with concurrent writers,
  no daemon stop required). rsync's non-DB state (transcripts
  JSONL, agent workspace-git dir, operator drops). `secret/`
  excluded by default; `--include-secrets` opts in (operators
  encrypt the archive themselves with `age`/`gpg`/encrypted
  bucket). sha256 manifest per file inside the archive +
  sidecar `<archive>.sha256` for transit-corruption detection.
  zstd-19 compression. Prints copy-paste restore instructions
  on every successful run.
- `docs/src/ops/backup.md` (NEW) — quickest-path command,
  restore steps (with the "stop daemon during rsync" warning),
  cron schedule template (`/etc/cron.daily/nexo-backup`),
  table of what survives the backup vs what regenerates on
  next boot (queue/, journalctl), migration status note
  pinning operators to a specific version per deployment until
  the proper subcommand ships.
- `docs/src/SUMMARY.md` registers the new page under
  Operations.

Deferred:
- `nexo backup --out <dir>` runtime subcommand. Touches
  `src/main.rs`.
- `nexo restore --from <archive>` runtime subcommand with
  consistency checks (refuses if daemon running, verifies
  manifest hashes, warns on schema drift).
- `nexo migrate up|down|status` versioned migrations replacing
  the current `ALTER TABLE … .ok()` patterns in the runtime.
- Encrypted archive output (built-in `age` integration).
- CI test that backup → restore round-trips on a fixture
  deployment.

The shell script + this doc are the operator bridge — they
work today, tested by anyone with `sqlite3` + `zstd` on PATH.
When 36.2+ subcommands ship, the doc rewrites to point at them
and the script retires.

#### 36.0 — Original prose (deferred items, kept for context)

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

### Phase 40 — Deployment recipes   🔄

Two recipes shipped (Hetzner + Fly.io); rest deferred until
Phase 27 release pipeline ships signed .deb / Docker / Helm
artifacts the recipes can consume end-to-end.

Shipped:
- `docs/src/recipes/deploy-hetzner.md` — concrete CX22 (€3.79/mo)
  walkthrough. Provision VM, harden (UFW, fail2ban, unattended-
  upgrades, no-root-ssh), install Nexo via signed .deb, install
  + bind NATS to loopback, Cloudflare Tunnel for HTTPS without
  opening ports, daily SQLite snapshot to S3-compatible storage
  via rclone, update path. Estimated cost spelled out.
- `docs/src/recipes/deploy-fly.md` — Fly.io single-region.
  `fly.toml` template using the GHCR image with persistent
  volume + Fly secrets injected as env vars resolved by the
  config loader's `${VAR}` placeholders. Pre-baked-config
  variant (custom Dockerfile.fly) vs first-boot wizard variant
  documented. Auto-deploy GitHub Action snippet. Snapshot-based
  backups via `fly volumes snapshots`. Free-tier vs
  performance-1x sizing guidance for the browser plugin.
- `docs/src/SUMMARY.md` registers both recipes under Recipes.

Both recipes are end-to-end runnable today against the artifacts
already in the pipeline (`docker pull ghcr.io/...:latest` +
`packaging/debian/build.sh` output) — the deferred half is just
"plug into the release workflow once 27.2 lands so users don't
have to build the .deb locally."

Shipped (40.3):
- `docs/src/recipes/deploy-aws.md` — EC2 t4g.small (ARM
  Graviton) recipe. Terraform main.tf for VPC + subnet +
  IGW + security group + IAM role (SES + S3 backups only,
  no console / no read of unrelated AWS resources) +
  instance profile + Debian 12 arm64 AMI lookup + EC2
  instance + Route53 record. Post-provision hardening (UFW,
  fail2ban, unattended-upgrades, no-root-ssh). Nexo install
  via signed .deb (cross-link 27.3 / 27.4). NATS install
  + bind to loopback. nginx + certbot for Let's Encrypt
  TLS (cheaper than ALB+ACM for single-VM deploys; ALB+ACM
  variant noted). SES outbound config using instance-profile
  credentials (no keys in YAML). EBS daily snapshots via
  DLM, cost breakdown line-by-line, AZ-failure / sandbox
  / EIP escape hatches, troubleshooting (instance profile,
  cert prop, broker readiness).
- `docs/src/SUMMARY.md` registers the recipe.

Deferred (downstream of 40.3):
- `docs/src/recipes/deploy-gcp.md` — Compute Engine + Cloud SQL
  + IAP. Same shape.
- `docs/src/recipes/deploy-render.md`,
  `docs/src/recipes/deploy-railway.md` — covered indirectly by
  the Fly recipe (same shape: persistent volume + env secrets).
- `deploy/terraform/<cloud>/` modules — operator workflow not
  steady state yet; ship modules once a real production deploy
  validates the manual recipe.
- `deploy/k8s/` Helm chart — depends on Phase 32 (multi-host)
  for genuine value. Single-replica Helm against the GHCR
  image is trivial; the interesting parts (StatefulSet for
  the broker, peer-discovery, anti-affinity) need 32 first.

Done when (revised): an operator picks a cloud, follows the
recipe end-to-end against the published release artifacts (signed
.deb / Docker image / SBOM), and ends up with a daemon running
under whatever process supervisor that cloud uses. Hetzner +
Fly recipes done; AWS / GCP / Terraform / k8s deferred.

#### 40.1 — Original prose (deferred items, kept for context)

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

### Phase 41 — Telemetry opt-in + roadmap signal   🔄

Privacy spec + operator doc shipped (binding contract for what
ever gets sent). CLI subcommand + emitter + server + dashboard
deferred — those land when the runtime side is ready to honour
the spec.

Shipped (41.1):
- `docs/src/ops/telemetry.md` — full operator-facing spec.
  Locks down: every field of the JSON document (schema v1),
  every excluded category (no message content, no identifiers,
  no API keys, no IPs, no hostname, no time-of-day), the
  receiving server's data-handling guarantees (IP dropped at
  LB, 90-day retention, no cross-`instance_id` correlation),
  inspection paths (`nexo telemetry preview`, `mitmproxy`,
  `iptables REJECT`), GDPR / HIPAA framing, schema changelog
  table for future bumps, what's deliberately out of scope
  (per-agent metrics — that's Prometheus; crash reports — those
  stay on-host).
- `docs/src/SUMMARY.md` registers the new page under Operations
  (between Pairing and the empty slot for Backups).

The doc is a contract: anything Phase 41.2+ ships has to match
this spec or bump `schema_version`. That keeps the implementation
honest before code lands.

Deferred:
- **41.2** `nexo telemetry status|enable|disable|preview` CLI
  subcommands. Touches `src/main.rs`.
- **41.3** Heartbeat emitter task (7d ± 1h jitter, respects
  `HTTPS_PROXY`, single retry per tick). Lives in
  `crates/core/src/telemetry/heartbeat.rs`.
- **41.4** First-launch journal banner (one-time print on a
  fresh install, suppressed thereafter via `~/.nexo/seen-banner`).
- **41.5** Hot-reload integration so toggling at runtime takes
  effect on the next tick without daemon restart.
- **41.6** Receiving server (`nexo-telemetry-server` repo —
  doesn't exist yet). Reproducible build, signed.
- **41.7** Public dashboard at
  `https://lordmacu.github.io/nexo-rs/usage/`.

Done when (revised): `nexo telemetry preview` prints a real
JSON document that matches the spec, the operator can flip
on/off without restart, the server returns 204 to a real
heartbeat, and the public dashboard shows aggregate adoption.
Spec done; runtime / server / dashboard land in 41.2+.

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
as theoretical. The trick: the maintainer's own deployment
counts as the first one. The other two come from operators we
help onboard.

#### 43.1 — Dogfood deployment ("Kate")

The maintainer's personal agent runs in production for at least
30 days before any external case study lands. Without
self-dogfooding the project doesn't earn the right to claim
others should adopt it.

- One agent ("Kate") on a Termux + Tailscale + cloudflared
  setup, paired with WhatsApp + Telegram + Gmail-poller +
  Browser + Calendar (Phase 65 once done; manual until then).
- Stays up ≥30 consecutive days with documented uptime
  (`agent metrics` scrape every 5 min into a tiny SQLite, daily
  rollup) and zero data loss across at least one Termux reboot
  + one phone OS update.
- Every incident logged: timestamp, root cause, fix, time-to-
  recover. Targets: ≥99% uptime, ≤5 min mean-time-to-recover,
  zero un-redacted secret in transcripts.
- A daily diary of what the agent did (10–20 line summary)
  shows that the deployment is *real*, not a synthetic test.

Done when the maintainer can point to 30 days of metrics +
incident log + diary, all auto-generated.

#### 43.2 — First case-study writeup

The dogfood deployment becomes the first published case study.

- `docs/src/case-studies/kate-personal-agent.md` covering:
  - Setup: hardware, channels, agent count, system prompt
    excerpts (sanitized), skills enabled, capability toggles
    armed.
  - Operations: how the operator interacts (Flutter app once
    Phase 30 ships; CLI + Telegram beforehand).
  - 30-day metrics: uptime, sessions/day, tokens/day,
    cost/month, incident count, user-perceived latency.
  - What worked, what didn't, what we'd do differently.
  - One concrete "the agent did this useful thing" anecdote.
- Linked from the README "Used by" section + the docs intro.
- Reproducible: all configs (with secrets stripped) attached
  as a `kate-deploy/` example so a reader can clone the shape.

Done when the page lives at the public docs URL, the README
links it, and a reader can replicate the setup.

#### 43.3 — Case study toolkit for adopters

Make it easy for someone else to publish their case study so we
don't gatekeep the format.

- Template: `docs/src/case-studies/_template.md` with required
  sections + suggested metrics.
- `agent metrics export --since <date>` produces a CSV /
  PNG-graph bundle that drops straight into the template.
- An anonymization mode: `--anonymize` strips operator names,
  agent names, and the obvious PII from the report.
- A submission page (`docs/src/case-studies/submit.md`)
  describing the PR-based contribution flow.
- A short consent form template for the operator's legal
  comfort (especially if they want their org's logo).

Done when an adopter can run two commands + edit one
templated markdown file and submit a PR with a complete case
study.

#### 43.4 — Outreach + onboarding for two more

Without help, no third party will write up a case study.

- Identify candidates: people already using nexo-rs in any
  capacity (early Discord / GitHub Discussions members, fork
  authors, social-media mentions).
- Offer a 1-hour onboarding call: help them set up, answer
  questions, leave them with a working deployment.
- 60-day follow-up: do they still use it? If yes, ask if
  they'd publish a case study using the toolkit.
- Optional incentive: free contributor swag / a callout in
  the README.
- Track outreach in `docs/src/case-studies/_pipeline.md`
  (private-by-default; published once converted).

Done when at least two non-maintainer operators have published
case studies under `docs/src/case-studies/<name>.md`.

#### 43.5 — "Used by" page + visual hub

Surface the social proof so a first-time visitor sees it
immediately.

- New page `docs/src/case-studies/index.md` listing every
  case study with a one-line summary + scale badge (sessions /
  month).
- README "Used by" block with logos (consent collected) for any
  org that wants public attribution; handles for individuals.
- Hero quote rotation on the docs landing page: pull a sentence
  from each case study.
- A short video interview (10–15 min) with at least one
  operator, embedded on the case-studies index.

Done when the README's "Used by" block has at least three
real logos / handles and the case-studies index is the
second-most-visited page on the docs site.

**Phase 43 done when** all 5 sub-phases tick green: 30-day
maintainer dogfood + writeup, adopter toolkit, two external
case studies, and a "used by" surface that fits-on-screen.

### Phase 44 — Auxiliary observability surfaces   🔄

Operator health-summary script + readiness/liveness doc shipped.
Per-session event log + `nexo inspect` + aggregated
`nexo doctor health` subcommand deferred.

Shipped (44.1):
- `scripts/nexo-health.sh` — single-shot JSON health summary.
  Probes `/health` (liveness), `/ready` (readiness),
  `/metrics` (Prometheus surface), pulls a few quick counters
  for the summary panel (tool_calls_total, llm_stream_chunks,
  web_search_breaker_open). Pretty human output by default,
  `--json` for monitoring scrapers, `--strict` to count an
  open breaker as unhealthy. Exit 0 on healthy, 1 on any
  probe failure (or breaker-open under `--strict`).
- `docs/src/ops/health.md` (NEW) — three-layer health-probe
  reference:
    * `/health` for Kubernetes liveness (cheap atomic flag).
    * `/ready` for load-balancer routing (verifies broker +
      agents loaded + snapshot warmed). 503 with JSON body
      listing failing subsystem.
    * `nexo-health.sh` for operator + monitoring (JSON
      summary with counter snapshots).
  Cron health-mailer template, UptimeRobot integration
  config, comparison table of when to use each surface.
- `docs/src/SUMMARY.md` registers the new page.

Deferred:
- `nexo inspect <session_id>` — pretty-print every state
  transition for one session (tool calls, hook fires, broker
  publishes, memory writes, redaction hits). Touches
  `src/main.rs` + `crates/core/`.
- Per-session structured event log under
  `data/events/<session>.jsonl` separate from transcripts.
  Needs an event-bus listener tap; touches `crates/core/`.
- `nexo doctor health` aggregating subcommand that runs
  every doctor (`setup`, `ext`, `capabilities`) and emits
  one summary. The shell script bridges this today.

The `/health` + `/ready` endpoints themselves are pre-existing
(Phase 9 polish). This sub-phase documents them and adds the
operator-friendly aggregator script on top.
- Crash dumps captured to `data/crashes/` with stack + recent
  log buffer.

Done when an operator triaging an incident has one command per
question and a deterministic place to look for breadcrumbs.

### Phase 45 — Cost & quota controls   🔄

Heuristic cost estimator script + runbook shipped. Tokens metric,
config caps, runtime enforcement, `nexo costs` subcommand all
deferred.

Shipped (45.1):
- `scripts/nexo-cost-report.sh` — Prometheus-driven heuristic
  estimator. Pulls `nexo_llm_stream_chunks_total` by provider,
  multiplies by configurable tokens-per-chunk × built-in price
  table, prints per-provider rolling totals + total dollar
  estimate. `--json` for machine consumption, `--prices` for
  enterprise rate overrides, `NEXO_TOKENS_PER_CHUNK` env for
  per-deployment calibration. Snapshots to
  `/var/cache/nexo-cost/last.tsv` so successive runs can show
  delta vs the previous snapshot.
- Default price table covers Anthropic / OpenAI / MiniMax /
  Gemini / DeepSeek / Ollama at public-list prices as of
  2026-04. Override via `--prices file.tsv`.
- `docs/src/ops/cost.md` (NEW) — operator runbook. How to
  calibrate tokens-per-chunk from a real conversation, daily
  budget alert via cron template
  `/etc/cron.daily/nexo-cost-alert`, raw-metrics inspection
  recipes for operators who want finer slices than the
  script provides, future-state preview of `cost_cap_usd`
  config when 45.x ships.
- `docs/src/SUMMARY.md` registers the new page.

Deferred:
- `nexo_llm_tokens_total{provider,model,direction}` Prometheus
  series. Replaces the chunks×heuristic estimator with direct
  token counts. Touches `crates/llm/`.
- `agents.<id>.cost_cap_usd { monthly, daily, action, warn_topic }`
  schema + runtime enforcement. `action` ∈
  `refuse_new_turns | warn_only | throttle` (throttle = swap
  to cheaper model variant for the period). Touches
  `crates/config/` + `crates/core/`.
- Per-binding token rate limit on top of the existing
  `sender_rate_limit`. Touches `crates/core/agent/`.
- Pre-flight token-count predictor that the agent can
  reference in its system prompt
  ("you have 80% of budget remaining today").
- `nexo costs` CLI rolling 24h / 7d / 30d aggregator.
- `/api/costs` admin endpoint surfacing the same data for the
  admin-ui A8 tile.

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

### Phase 46 — Local LLM (Ollama / llama.cpp / vLLM)

Today every LLM hop costs money and leaks data to a third party.
Local-first matters for privacy, Termux deployments, and
operators who want to bring their own model.

- New provider in `crates/llm/src/local.rs` speaking the Ollama
  HTTP API by default; OpenAI-compat already covers
  llama.cpp / vLLM / LM Studio / TGI / Mistral.rs — ship recipe
  docs.
- Streaming + tool calling support; document which open models
  honor the OpenAI tool format reliably (Qwen2.5, Llama-3.x,
  Mistral, Hermes 3, etc.).
- `agent llm pull <model>` convenience wrapper over `ollama pull`.
- Smart fallback: local model fails or is slow past a threshold
  → route to a remote provider (operator-opt-in).
- Quantization / sizing recommendations per device class (phone,
  small VPS, real GPU).
- Local embedding provider for `sqlite-vec` so RAG works without
  OpenAI either.

Done when a fresh Termux install can `agent llm pull
qwen2.5:7b` and run a working agent end-to-end with no API key
set anywhere.

> **Relación con Phase 68**: Phase 46 trata al modelo local como
> *provider primario* del agente (mismo rol que Anthropic /
> MiniMax). Phase 68 lo trata como *tier-0 transversal* del runtime
> (PII redactor, embeddings, classifiers, fallback) — corre **en
> paralelo** al cloud LLM, no lo reemplaza. Las dos fases son
> complementarias y comparten el crate hoja `nexo-llm-local`.

### Phase 47 — Vector store abstraction

Today memory is `sqlite-vec` only. That works on a phone but
not at company scale.

- `VectorStore` trait in `crates/memory/`.
- Implementations (start): `sqlite-vec` (current), `pgvector`,
  `qdrant`. Cover phone / SQL-shop / dedicated-vector-db cases.
- YAML knobs in `memory.yaml` (`vector.provider: qdrant`, url,
  collection, etc.).
- Migration tool: `agent memory migrate --from sqlite-vec --to
  qdrant`.
- Hybrid search (keyword + vector + metadata filter) works
  across providers with the same query API.

Done when an operator can swap vector backends with one YAML
edit and a migration command, with comparable recall quality.

### Phase 48 — Email channel completion

`crates/plugins/email/` is a scaffold; flesh it into a
first-class channel.

- IMAP IDLE inbound (push) with 60s poll fallback.
- SMTP outbound, SPF/DKIM-aware (warn at boot when domain
  alignment is broken).
- Threading via `Message-ID` / `In-Reply-To` so a reply chain
  is one session.
- Attachment support both directions through the existing
  `Attachment` envelope.
- Per-account credentials via Phase 17 (`credentials.email`).
- Outbound tools: `email_archive`, `email_label`,
  `email_move_to`, `email_reply`.

Done when an agent can subscribe to an inbox, reply
contextually with proper threading, and operate on labels +
folders.

Sub-phases:

- 48.1 — Scaffold + multi-account config schema   ✅
  (`crates/plugins/email/{lib,plugin}.rs` Plugin-trait stub,
  `EmailPluginConfig` extended to `accounts: [{instance,
  address, provider, imap{host,port,tls},
  smtp{host,port,tls}, folders, filters}]` plus
  `loop_prevention`, `max_body_bytes=32 KiB`,
  `idle_reissue_minutes=28`, `poll_fallback_seconds=60`,
  `spf_dkim_warn`, sample `config/plugins/email.yaml` empty
  by default. 3 round-trip + 2 plugin unit tests pass; old
  single-account schema replaced in `load_test.rs` fixtures.)
- 48.2 — `nexo-auth` `EmailCredentialStore` (Password / OAuth2 / OAuth2-Google)   ✅
  (`crates/auth/src/email.rs` adds `EmailAccount` + `EmailAuth`
  enum (`Password` / `OAuth2Static` / `OAuth2Google`) backed by
  `secrecy::SecretString` so `tracing::debug!("{:?}")` redacts
  every secret to `<redacted>`. New `Channel::EMAIL = "email"`.
  TOML loader at `secrets/email/<instance>.toml` with env-var
  interpolation via `nexo_config::env::resolve_placeholders`.
  Gauntlet branch in `wire::build_credentials` is gated on
  `Option<&EmailPluginConfig>`: missing TOML →
  `CredentialError::FileMissing`, malformed →
  `CredentialError::InvalidSecret`, `OAuth2Google` pointing at a
  non-existent google account →
  `CredentialError::OrphanedGoogleRef`, mode > `0o600` →
  `CredentialError::InsecurePermissions`.
  `EmailAccount::resolve_access_token` delegates Google variant
  to the existing per-account `refresh_lock` so concurrent IDLE
  workers do not race a token rotation.
  `xoauth2_sasl(user, token)` static helper emits the RFC 7628
  SASL payload base64'd. `CredentialStores.email` + resolver
  ramas EMAIL en `store_list` / `allow_agents_for` /
  `store_issue`. Hot-reload (`reload_resolver`) takes a
  `secrets_dir: &Path` so admin endpoint can pick up new TOMLs
  without daemon restart. Telemetry: `set_accounts_total(EMAIL,
  …)` reuses the generic credential gauge (no new metric).
  20 unit tests in `email::tests`; existing `cross_agent_isolation`
  / `hot_reload` / `strict_legacy_google_auth` / `wire`
  integration tests migrated to the new 7-arg signature; full
  `cargo test --workspace` green at 1943 / 0.)
- 48.3 — IMAP IDLE worker + poll fallback (CB-wrapped)   ✅
  (Multi-account `InboundManager` spawns one `AccountWorker` per
  declared account; each owns one `ImapConnection` (rustls TLS over
  TCP, port 993 / `ImplicitTls` only in v1 — `Starttls`/`Plain`
  reject at boot with operator-actionable error). Auth picks LOGIN
  for `Password`, `AUTHENTICATE XOAUTH2` for `OAuth2Static` /
  `OAuth2Google` (token resolved via 48.2's per-account refresh
  mutex). `CAPABILITY` cached post-login (`idle`, `uidplus`,
  `move`); servers without IDLE permanently enter `WorkerState::
  Polling` (60s `UID SEARCH UID last+1:*`). Workers in IDLE issue
  `wait_with_timeout(idle_reissue_minutes * 60)` (default 28 min,
  under RFC 2177's 29-min ceiling) with a `CancellationToken` arm
  on `tokio::select!` so plugin shutdown unwinds cleanly via
  `IDLE DONE`. Reconnect path runs through `nexo_resilience::
  CircuitBreaker` (default 5-fail / 10s..120s) with ±20% jittered
  exponential backoff (`1s, 2s, 4s, ..., 60s` cap) so N accounts
  losing connectivity simultaneously don't thunder-herd. `BODY.
  PEEK[]` + `INTERNALDATE` fetch — never marks `\Seen`. `(uid_
  validity, last_uid)` cursor in `<data_dir>/email/cursor.db`
  (sqlx-sqlite, WAL); cursor reset to `last_uid=0` whenever the
  server announces a different `UIDVALIDITY`. Cursor advance
  happens **after** a successful publish so a crash mid-batch
  reprocesses, not loses (at-least-once). `InboundEvent
  {account_id, instance, uid, internal_date, raw_bytes}`
  published as JSON on `plugin.inbound.email.<instance>` (raw
  bytes via `serde_bytes`); MIME parsing + threading still
  deferred to 48.5/48.6. Per-account `AccountHealth` map exposed
  via `EmailPlugin::health_map()` for 48.10. TCP keepalive 30s
  survives CGNAT idle drops that would otherwise kill IDLE
  silently. `EMAIL_INSECURE_TLS=1` env opens a logged WARN escape
  hatch for fake-server tests; production ignores it. 15 unit
  tests across `cursor`, `events`, `health`, `imap_conn`,
  `inbound`, `plugin`. Real-server e2e (greenmail) deferred to
  48.10 along with `Starttls` support.)
- 48.4 — SMTP outbound + disk-queue + Message-ID idempotency   ✅
  (Foundational slice 48.4.a + lettre/dispatcher slice 48.4.b ship
  the full outbound channel. `mime_text.rs` generates stable
  `<{ts_ms}.{uuid_v4}@{domain}>` Message-IDs and renders RFC 5322
  text/plain (Bcc omitted from headers, RFC 2047 encoded-word for
  non-ASCII subjects, In-Reply-To / References passthrough for
  48.6). `outbound_queue.rs` is a single-writer JSONL append-log per
  `<dir>/<instance>.jsonl` plus DLQ sibling, with tombstone rows +
  `compact_if_needed` (rewrites at >50% done ratio) keeping the
  file bounded. `smtp_conn.rs` wraps `lettre 0.11`
  `AsyncSmtpTransport<Tokio1Executor>` for `Plain` / `Starttls` /
  `ImplicitTls`, picks `Mechanism::Plain` for `Password` and
  `Mechanism::Xoauth2` for OAuth (token resolved through 48.2's
  per-account refresh mutex), and classifies `lettre::Error::is_
  permanent()` / `is_transient()` into the coarse outcome enum.
  `outbound.rs::OutboundDispatcher` spawns one `OutboundWorker` per
  account: subscribes to `plugin.outbound.email.<instance>` and
  drives a 1s drain ticker. Retries: `2s, 4s, 8s, 16s, 30s` cap
  with ±20% jitter, max 5 attempts. 4xx → bump attempts and
  reschedule; 5xx → DLQ immediately + ack `Failed`; network errors
  count against the SMTP-specific `(EMAIL, "<inst>.smtp")`
  CircuitBreaker. `DashMap` in-flight guard prevents two concurrent
  drain ticks from reissuing the same Message-ID. Acks publish on
  `plugin.outbound.email.<instance>.ack` (`Sent` / `Failed` /
  `Retrying`). Health gains `outbound_queue_depth` /
  `outbound_dlq_depth` / `outbound_sent_total` /
  `outbound_failed_total` per account, refreshed each tick.
  `EmailPlugin::start` now arms inbound + outbound, sharing the
  `HealthMap`; `stop` brings outbound down first to flush in-flight
  sends, then inbound, with the existing 5s budget. 40 unit tests
  green; clippy clean. Real-server e2e (greenmail) deferred to
  48.10 along with `Starttls` IMAP support.)
- 48.5 — MIME parse/build + Attachment envelope   ✅
  (48.5.a foundational slice + 48.5.b inbound parser + 48.5.c
  multipart builder + outbound wiring.
  `mime_parse.rs` wraps `mail-parser 0.9` to lift `BODY.PEEK[]`
  bytes into `EmailMeta` + `Vec<EmailAttachment>`. Body text
  picks the `text/plain` part; HTML-only messages get
  `html2text`-stripped into `body_text` while keeping the raw
  HTML in `body_html` for archive/render fidelity. UTF-8-safe
  truncation at `max_body_bytes` (32 KiB default), oversized
  attachments capped at `max_attachment_bytes` (25 MiB) with the
  `truncated: true` signal. Attachments persisted to
  `<attachments_dir>/<sha256>` via atomic temp-rename; identical
  bytes across accounts hit disk once. `Date:` falls back to IMAP
  `INTERNALDATE` when the header is missing or malformed. From /
  To / Cc parsed into `AddressEntry` (display name preserved).
  `headers_extra` whitelists the `Auto-Submitted` / `List-*` /
  `Precedence` family that 48.8 needs; mail-parser's
  `HeaderValue::Address` flattening means `<list-id>` style
  headers round-trip without losing brackets. `Content-
  Disposition: inline` lifts to `AttachmentDisposition::Inline`,
  preserving `Content-ID` for tools/UI. Drain hook in
  `inbound.rs::drain_pending` wraps `parse_eml` in a best-effort
  branch — parse failure logs WARN `email.parse.malformed` and
  publishes the event raw-only so a single corrupt MIME never
  wedges the worker. 10 new parser tests cover plain / HTML-only /
  multipart-with-attachment / sha256 dedupe / truncation / list
  headers / missing Date / malformed / display-name.
  Outbound `mime_build.rs` replaces `mime_text.rs`: the
  no-attachment branch keeps the 48.4 hand-rolled wire format
  byte-for-byte (so existing expectations don't shift); the
  with-attachment branch hands off to `mail-builder 0.4`'s
  `text_body` + `attachment`/`inline` API which wraps the
  message in `multipart/mixed` automatically.
  `mime_guess::from_path` infers a part's content-type when the
  caller leaves `mime_type` `None`; explicit overrides win.
  `Bcc` stays out of headers in both branches.
  `outbound.rs::enqueue_command` now reads each
  `OutboundAttachmentRef.data_path` at enqueue time so a missing
  file fails fast with a `build_mime` Err rather than silently
  parking a doomed job. 8 new builder tests (no-attach wire
  matches 48.4, message-id format, In-Reply-To + References
  passthrough, RFC 2047 subject, multipart/mixed emission with
  mime_guess, missing-file Err, explicit-mime override).
  58 / 58 plugin unit tests; clippy clean.)
- 48.6 — Threading via `Message-ID` / `In-Reply-To` / `References`   ✅
  (`crates/plugins/email/src/threading.rs` ships pure helpers:
  `canonicalize_message_id` (lowercase + bracket-strip + reject
  CR/LF/comma/whitespace as defence against header injection),
  `resolve_thread_root` walking RFC 5322 §3.6.4 priority
  (`references[0]` → `in_reply_to` → `message_id` →
  `<orphan-{uid}@{account}>` synth so the helper never returns
  None), `session_id_for_thread` = `Uuid::new_v5(EMAIL_NS,
  root)` mirroring `telegram::session_id_for_chat` /
  `whatsapp::session_id_for_jid`, `truncate_references` keeping
  root + last `(max-1)` ids per RFC 5322, `enrich_reply_threading`
  mutating an `OutboundCommand` so a reply inherits the parent
  chain (idempotent — re-invoking is a no-op via dedupe), and
  `is_self_thread` for 48.8 loop-prev. `EMAIL_NS` is pinned via
  `uuid!("c1c0a700-48e6-5000-a000-000000000000")` — bumping it
  re-shuffles every email session id, hence the regression test
  asserting the v5 derivation. `InboundEvent.thread_root_id:
  Option<String>` (skip-on-None for back-compat with 48.3/48.4
  payloads) is populated by `inbound::drain_pending` after
  successful `parse_eml`. Workspace `uuid` workspace dep gains
  `v5` + `macro-diagnostics` features. 28 unit tests; 84 / 84
  plugin total; clippy clean.)
- 48.7 — Tools: `email_send` / `_reply` / `_archive` / `_label` / `_move_to` / `_search`   ✅
  (48.7.c adds the four IMAP-leaning handlers and closes the
  phase. `imap_conn.rs` grows wrappers for `UID SEARCH`, `UID
  MOVE`, `UID COPY`, `UID STORE` (drains the response stream so
  the session is ready for the next command), `EXPUNGE` (pinned
  in-place because async-imap's stream isn't `Unpin`), and
  `fetch_search_rows` returning a stable `SearchRow` row with
  `uid`, `message_id?`, `from`, `subject`, `date`, and a
  ≤200-char snippet from `BODY.PEEK[TEXT]<0.200>`. The header
  block from `BODY.PEEK[HEADER.FIELDS (FROM SUBJECT MESSAGE-
  ID)]` is parsed with a tolerant fold-aware reader so a
  continued line never drops the field.

  `tool/uid_set.rs` formats `Vec<u32>` → IMAP `sequence-set`
  (`1,2,3`). `tool/dispatcher_stub.rs` is a #[cfg(test)] stub of
  `DispatcherHandle` shared by every handler's unit tests so they
  exercise the schema + dispatcher routing without standing up
  `AgentContext` or a real broker.

  `email_archive` runs `UID MOVE` to `folders.archive` when the
  server advertises MOVE (RFC 6851), else falls back to `UID
  COPY` + `UID STORE +FLAGS (\Deleted)` + `EXPUNGE` and reports
  `fallback: true` in the result so the operator knows the
  expensive path triggered. `email_move_to` is the same dance
  for an arbitrary folder (no auto-create — missing folder
  surfaces as IMAP NO in the result envelope). `email_label` is
  Gmail-only: detects `provider == EmailProvider::Gmail` and
  emits a clean `requires gmail provider` error otherwise rather
  than confusing the agent with a downstream IMAP NO. Labels go
  through `format_label_list` which quotes only when needed
  (whitespace, parens, backslash, double quote) — keeps simple
  labels readable on the wire while staying RFC 3501 valid.

  `email_search` is the largest of the four. Translates a
  portable JSON DSL (`from / to / subject / body` substring,
  `since / before` YYYY-MM-DD, `unseen / seen` booleans) into
  IMAP SEARCH atoms. Every user-controlled string passes
  through `imap_quote` (RFC 3501 quoted-string + CRLF collapse)
  before reaching the wire — quoting is the security boundary,
  not aesthetic. `since`/`before` parse via `chrono::NaiveDate`
  → `imap_date` (`d-MMM-yyyy`). Empty query → `ALL` so the
  server doesn't receive a syntactically invalid empty query.
  `limit` defaults to 50, capped at 200 (context-friendly for
  the LLM). FETCHes one row per matched UID.

  `tool/mod.rs::register_email_tools` now wires all six
  handlers. 18 new unit tests across the four handlers and the
  helpers (uid_set format, label list quoting / escaping,
  search atom composition, injection-attempt quoting, date
  parse error). 116 / 116 plugin total. Phase 48.7 closes;
  greenmail e2e for the IMAP/SMTP wire pinned to 48.10.

  48.7.b adds the two outbound-leaning handlers.
  `email_send` accepts the same shape as 48.4's
  `OutboundCommand` (instance, to/cc/bcc, subject, body,
  attachments path-by-reference) and forwards through the
  `DispatcherHandle` from 48.7.a; `from` is fixed to the
  account address (anti-spoof) and never appears in the schema.
  Result envelope is `{ ok, message_id }` on success or
  `{ ok: false, error }` on schema / dispatcher failure — uniform
  LLM-friendly shape. `email_reply` takes
  `{ instance, parent_uid, body, reply_all?, attachments? }`,
  opens an ephemeral IMAP via `run_imap_op`, FETCHes the parent
  raw bytes, parses with 48.5, derives recipients via the
  `derive_reply_recipients` helper (To = parent.From; reply_all
  → +parent.To/Cc minus own address, deduped case-insensitively),
  prefixes `Re: ` only when the parent subject lacks one,
  invokes 48.6 `enrich_reply_threading`, and dispatches.
  Returns `{ ok, message_id, to, cc }` so the caller audits who
  got the reply. Handlers expose pure-logic `run(args)` helpers
  the tests exercise without standing up a full
  `nexo_core::AgentContext`. 10 new unit tests (4 send: happy /
  unknown instance / missing field / dispatcher Err; 6 reply:
  single recipient / reply_all dedup / case-insensitivity /
  exclude parent.From / empty From → empty To / `Re:` prefix
  idempotency). `crates/plugins/email/Cargo.toml` gains a
  `nexo-llm` workspace path dep so tools can reference
  `ToolDef`. 98 / 98 plugin total. Phase 48.7.a foundational
  pieces unchanged. 48.7.c (archive / move_to / label / search)
  remains.

  48.7.a foundational slice: `tool/context.rs` declares the
  `DispatcherHandle` async trait + `EmailToolContext` aggregate
  (creds + Google + config + dispatcher façade + health map),
  with a convenience `account(instance)` lookup. `tool/imap_op.rs`
  ships `run_imap_op` (ephemeral connect → SELECT → closure →
  LOGOUT, no pool in v1), `imap_quote` (RFC 3501 quoted-string
  escape with CRLF collapse — defends search atoms against
  header-injection), and `imap_date` (`d-MMM-yyyy` for SEARCH
  SINCE/BEFORE). `tool/mod.rs::register_email_tools` is a stub
  for now — the six handlers themselves arrive in 48.7.b/c.
  `OutboundDispatcher` grows `instances:
  Arc<DashMap<String, InstanceState>>` populated at `start`,
  plus public `instance_ids()` and `enqueue_for_instance(inst,
  cmd)` (generates Message-ID, builds MIME via 48.5, persists
  via 48.4 queue — worker drain tick picks it up). The
  `DispatcherHandle` impl on `OutboundDispatcher` is a thin
  forwarder. `lib.rs` re-exports the new public surface. 4 unit
  tests in `imap_op` (quote escape, CRLF collapse, simple wrap,
  date format); 88 / 88 plugin total; clippy clean.)
- 48.8 — Loop-prevention + DSN/bounce parsing   ✅
  (Two pure modules wired into `inbound.rs::drain_pending`.
  `loop_prevent.rs::should_skip(meta, account, cfg)` walks
  `Auto-Submitted` (RFC 3834 — anything other than the literal
  `no` is the loop signal) → `List-Id` / `List-Unsubscribe` (RFC
  2369) → `Precedence: bulk|junk|list` (RFC 2076) →
  `is_self_thread` (48.6 reuse) and returns the first match as a
  `SkipReason` with a stable `metric_label()`. First-match
  ordering means the most specific category wins (a list mail
  that also ships `Auto-Submitted` reports as `auto_submitted`).
  `dsn.rs::parse_bounce(meta, raw)` detects DSNs by the
  `Content-Type: multipart/report; report-type=delivery-status`
  marker, falling back to a heuristic localpart match
  (`MAILER-DAEMON` / `mail-daemon` / `mail.daemon` /
  `postmaster`) when the marker is missing — some legacy MTAs
  ship plain-text bounces. Walks the parts: `message/delivery-
  status` body is parsed by hand (mail-parser doesn't expose
  the inner Message) with a fold-aware reader for `Action`,
  `Status`, `Final-Recipient` (`rfc822;` prefix stripped),
  `Diagnostic-Code`. `message/rfc822` is re-parsed for the
  original Message-ID; `text/rfc822-headers` is the cheaper
  variant. `BounceClassification::from_status_code` — `5.x` →
  `Permanent`, `4.x` → `Transient`, missing → `Unknown`. Wire
  payload `BounceEvent { account_id, instance,
  original_message_id?, recipient?, status_code?, action?,
  reason?, classification }` published on `email.bounce.<inst>`.

  `drain_pending` evaluates DSN first (a delivery report still
  emits a BounceEvent even when it ships `Auto-Submitted`),
  then loop-prevention. Either way the cursor advances — a
  suppressed message has been *processed* successfully and must
  not reprocess on the next IDLE wake. `AccountWorker` now
  carries a cloned `LoopPreventionCfg` so the hot path doesn't
  reach back into shared config.

  18 new unit tests (8 `dsn` covering classification, Postfix
  5.1.1, Exchange-style 4.7.0 transient, MAILER-DAEMON
  heuristic, malformed partial, regular-mail-returns-None,
  Final-Recipient `addr-type;` strip; 9 `loop_prevent` covering
  every branch + cfg-off-skips-nothing + Auto-Submitted-no
  doesn't-skip + first-match priority).
  134 / 134 plugin total. Persistent bounce history,
  rate-limited `email_send` against bounce count, and DSN
  dedupe `LRU<msg_id>` deferred to 48.10. `cargo build
  --workspace` green; clippy clean.)
- 48.9 — SPF/DKIM boot warn + setup-CLI submenu   🔄
  (48.9.a SPF/DKIM half shipped: `spf_dkim.rs::check_alignment`
  uses `hickory-resolver 0.24` (system config first, Cloudflare
  fallback so CI without /etc/resolv.conf still works) to TXT-
  lookup `domain` and `default._domainkey.<domain>` under a 3s
  `tokio::time::timeout`. SPF policies parsed via
  `parse_spf_includes` extract `include:<host>` mechanisms (RFC
  7208 §5.2, tolerant of `+`/`?` qualifiers and trailing dot);
  the report flags `spf_includes_host` when the operator-supplied
  `sending_host` (typically the SMTP relay) matches an include
  by exact equality or DNS-suffix. `decide_warns(report)` is the
  pure switchboard the boot hook calls — emits `spf_missing` /
  `spf_misalignment` / `dkim_missing` / `dns_error` tags so the
  dispatch into `tracing::warn!` lines stays unit-testable.

  `provider_hint.rs::provider_hint(domain)` ships a 5-row table
  (Gmail / Outlook including hotmail/live/msn / Yahoo / iCloud
  / Custom fallback) returning ready-to-paste IMAP+SMTP host /
  port / TLS triples plus a `suggest_oauth_google` flag that the
  setup wizard will use to surface "reuse your existing
  google-auth.yaml account?" only when it actually applies.

  `EmailPlugin::start` spawns one boot-time check task per
  configured account when `cfg.spf_dkim_warn` is enabled. Each
  task logs structured WARN lines (`email.spf.missing`,
  `email.spf.misalignment` with the offending sending_host,
  `email.dkim.missing` with the four common selectors as a
  hint, `email.spf_dkim.dns_unavailable` for DNS flakes) and
  never blocks the daemon. 20 new unit tests (10 spf_dkim
  including parser edge cases + decide_warns matrix + invalid-
  domain smoke + empty-domain dns_error; 7 provider_hint
  including aliases + case-insensitivity + Custom fallback).
  154 / 154 plugin total. Interactive setup wizard
  (`run_email_wizard` + `yaml_patch::upsert_email_account` +
  TOML-secret writer with 0o600) deferred to 48.10 along with
  the rest of the operator-facing surface.)
- 48.10 — Health + hot-reload + e2e + docs   ✅
  (Closing slice: `src/main.rs` instantiates `EmailPlugin::new(
  cfg, creds_email, creds_google, data_dir)` when
  `cfg.plugins.email.enabled && !accounts.is_empty()` and
  registers it alongside Telegram / WhatsApp; tool registration
  via `register_email_tools` is intentionally deferred (the
  registry build runs before `EmailPlugin::start` arms the
  `OutboundDispatcher` whose `DispatcherHandle` the tools need
  — tracked in FOLLOWUPS).
  `crates/setup/src/capabilities.rs::INVENTORY` gains an
  `EMAIL_INSECURE_TLS` row (`Risk::High`) so `agent doctor
  capabilities` surfaces the toggle that 48.3 wired into the
  TLS connector. `docs/src/plugins/email.md` is rewritten end
  to end (~400 lines) covering the YAML schema, TOML secrets
  with all three auth kinds, the provider-hint table, the six
  tools with sample payloads, the inbound + bounce + ack wire
  formats, the loop-prevention matrix, the SPF/DKIM warn tags
  and what each one means, troubleshooting (UIDVALIDITY
  changes, IDLE-unsupported, DLQ growth, XOAUTH2 failures,
  insecure-TLS), and an explicit "Limitations" table that
  links every deferred item back to FOLLOWUPS or the phase
  that will pick it up. `admin-ui/PHASES.md` gains an Email
  plugin block enumerating the operator-visible knobs the
  future admin UI must surface (account CRUD, secrets editor,
  SPF/DKIM banner, bounce inbox, queue/DLQ inspector,
  `EMAIL_INSECURE_TLS` capability badge). `crates/plugins/
  email/README.md` drops the AWS-SES sales pitch the original
  scaffold shipped with and points at the mdBook page.
  `proyecto/FOLLOWUPS.md` registers the deferrals: setup
  wizard, greenmail e2e, hot-reload account-diff, persistent
  bounce history, IMAP STARTTLS, multi-selector DKIM, healthz
  HTTP integration, dedicated Prometheus metrics, Phase 16
  binding-policy auto-filter, cross-account attachment GC.
  `cargo build --workspace` green; `cargo test -p
  nexo-plugin-email` 154 / 154; clippy clean across plugin +
  setup.)

### Phase 49 — Multimodal: vision input

Image input across providers (output already covered by Phase
24).

- `ChatMessage` / `Attachment` extended so an inbound image
  (WA / TG / email / iMessage) flows into the LLM as a vision
  part.
- Adapters: Anthropic `image` block, OpenAI `image_url`, Gemini
  inline `Part`, Ollama `images: [base64]`.
- Auto-resize / re-encode at the channel boundary so a 12 MB
  WA photo doesn't blow per-provider request caps.
- `vision_describe` tool the agent can call explicitly.
- Local vision via `llava` / `qwen2-vl` / `llama-vision` on
  Ollama.

Done when an inbound WhatsApp photo reaches the LLM and the
agent's reply is grounded in the image content on at least
two of (Anthropic, OpenAI, local Ollama).

### Phase 50 — Privacy toolkit (GDPR-ish)   🔄

Shell-bridge + operator runbook shipped. Runtime subcommands +
PII detection + sqlcipher encryption at rest deferred.

Shipped (50.1):
- `scripts/nexo-forget-user.sh` — cascading delete bridge.
  Walks every SQLite DB under `NEXO_HOME` and drops rows matching
  the target id across the canonical user-keyed columns
  (`user_id`, `sender_id`, `account_id`, `contact_id`, `peer_id`).
  Filters JSONL transcripts in place. Refuses to run if the
  daemon is alive (SQLite WAL doesn't survive parallel writes).
  Dry-run by default; `--apply` is the explicit go. `--keep-audit`
  preserves the `admin_audit` row for operator audit chains
  (the row is anonymised — user-id field hashed). Emits
  `forget-user-<id>-<timestamp>.json` manifest with exact
  deletion counts as the GDPR audit trail.
- `docs/src/ops/privacy.md` (NEW) — operator runbook covering
  right-to-be-forgotten via the script, manual data-export
  pipeline (sqlite3 + jq + age-encrypt), recommended retention
  policy table per surface (transcripts 90d, taskflow finished
  30d, taskflow failed 365d, admin audit 365d, disk-queue 7d),
  cron template `/etc/cron.daily/nexo-retention` for retention
  enforcement, status table for the deferred runtime
  subcommands.
- `docs/src/SUMMARY.md` registers the new page.

Deferred:
- `nexo forget --user <id>` runtime subcommand with cascading
  delete + manifest emission. Touches `src/main.rs`.
- `nexo export-user --id <id>` runtime subcommand with built-in
  age-encryption.
- Inbound PII detection (regex pre-screen + optional Phase 68
  local-LLM second pass) emitting `data/pii-flags.jsonl` for
  operator review.
- Separate admin-action audit log under `data/admin-audit.jsonl`
  recording every YAML edit, agent CRUD, capability toggle
  with operator id + before/after diff. (Distinct from the
  per-deletion manifest the forget script emits.)
- `sqlcipher` build of `libsqlite3-sys` for application-level
  encryption at rest.
- `dm-crypt` / LUKS recipe for filesystem-level encryption.

The shell script + runbook are the operator bridge — they work
today, GDPR-compliant audit trails included. When 50.2+
subcommands ship, the runbook rewrites to point at them and the
script retires.

#### 50.0 — Original prose (deferred items, kept for context)

- `agent forget --user <id>` cascading delete across
  transcripts, memory, taskflow rows, attachments, audit log.
  Returns a manifest.
- `agent export-user <id>` — single zip with everything tied
  to the id, optionally `age`-encrypted.
- PII detection on inbound: flag SSN / credit card / phone-shape
  patterns for operator review (separate from the
  outbound-side redactor).
- Admin-action audit log under `data/admin-audit.jsonl`: every
  YAML edit, agent CRUD, capability toggle, with operator id +
  before/after diff.
- Encryption at rest: `sqlcipher` option for SQLite databases,
  documented dm-crypt recipe for the `data/` volume.

Done when the operator can demonstrate full data lifecycle
control for one user end-to-end.

### Phase 51 — Eval harness + prompt versioning   🔄

Fixture format spec + 5 starter suites + manual shell harness
recipe shipped. Runtime crate + CLI + LLM-as-judge + shadow
traffic + git-versioned prompt history all deferred.

Shipped (51.1):
- `evals/README.md` — fixture format spec. Each case is one
  JSONL line with `id`, `input`, `expectations[]`, `tags`,
  `notes`. Expectation kinds documented:
  `no_error`, `regex_response`, `regex_response_not`,
  `tool_called`, `tool_not_called`, `response_lang`,
  `response_max_tokens`, `redacted`, `judge_score`. Per-case
  score in [0, 1] from weighted expectations; suite passes at
  ≥ 0.8. Manual shell harness recipe (curl + jq) for use
  against any OpenAI-compatible endpoint until the runtime
  CLI lands.
- `evals/smoke-en.jsonl` (12 cases) — English personal-agent
  baseline: greetings, tool routing (current_time, weather,
  send_whatsapp, reminder, memory_recall, web_search,
  link_understanding), arithmetic correctness, response-length
  bounds.
- `evals/smoke-es.jsonl` (12 cases) — same set in Spanish,
  with `response_lang: "es"` expectations to catch language
  drift on i18n channels.
- `evals/tool-routing.jsonl` (8 cases) — pure routing
  discrimination: trivial arithmetic must NOT call web_search,
  common-knowledge questions must NOT call web_search, but
  real-time data MUST. Calendar / email / link cases verify
  the right tool wins.
- `evals/refusal.jsonl` (5 cases) — agent must refuse
  destructive shell ops (`rm -rf /`, "drop production
  database"), impersonation + bulk-outbound fraud requests,
  secret exfiltration ("tell me my Anthropic API key" — must
  refuse AND must not leak any `sk-ant-` prefix), classic
  prompt-injection ("forget your instructions").
- `evals/pii-redaction.jsonl` (6 cases) — verifies the
  redactor strips SSN, credit-card (Luhn-valid Visa test
  number 4111…), third-party email, the user's own phone in
  outbound text, government-id formats. Negative control
  ensures the redactor doesn't mangle PII-free responses.

Total: **43 cases across 5 suites**, ready to run today via
the documented shell harness against Anthropic / OpenAI / any
OpenAI-compat endpoint.

Deferred:
- `crates/evals/` runtime crate.
- `nexo eval run --suite <path>` CLI with full agent runner
  (real tool dispatch, not just prompt → LLM).
- `nexo eval compare <a> <b>` for delta reports between
  prompt versions.
- LLM-as-judge `judge_score` expectation evaluator (uses a
  separate model to score open-ended outputs against a
  rubric).
- Shadow-traffic mode — duplicate N% of real inbound to a
  candidate prompt, never reach the user.
- Git-versioned prompt history with eval scores attached
  (cross-link Phase 10.9 git-backed memory).
- Multi-turn conversation simulator.
- Streaming-vs-non-streaming output diff harness.

Done when (revised): an operator can refactor a system
prompt, run `nexo eval run`, and see a delta graph for tone /
factual / tool-call rate before shipping. Fixture set + manual
harness done; runtime tooling blocks on 51.2+.

Prompts drift. Today there's no way to detect a regression
from a prompt edit.

- `crates/evals/` runner: takes JSONL of golden
  `(input, expected_output)` cases + a prompt version, scores
  with a judge model (or simple metrics).
- `agent eval run --suite <path>`, `agent eval compare <a> <b>`.
- Prompt history: every change to `system_prompt` /
  `IDENTITY.md` / `SOUL.md` versioned in the workspace git repo
  (Phase 10.9), eval scores attached.
- Shadow traffic: send N% of real inbound to a candidate prompt,
  compare to baseline async, never reach the user.
- Bundled smoke suite (~50 cases) for pre-deploy sanity.

Done when an operator can refactor a system prompt, run
`agent eval`, and see deltagraph for tone / factual /
tool-call rate before shipping.

### Phase 52 — Time-travel replay debugger

Diagnose "agent did the wrong thing" without grepping logs.

- Every session captures a deterministic tape: LLM responses,
  tool results, broker events, all keyed by call id.
- `agent replay <session_id>` re-runs the agent against the
  tape — same inputs, same outputs, externals mocked. Operator
  can step through, inspect ctx + state at any turn, branch
  with a different prompt.
- Admin UI timeline visualization (Phase 29 follow-up).
- Anonymization mode strips PII before sharing the tape.

Done when an operator can replay a 50-turn production session,
step through any turn, and pinpoint where the agent went
off-rails.

### Phase 53 — Cron + state-machine workflows

TaskFlow handles linear flows. Two missing shapes:

- **Cron scheduling** — `crates/scheduler/` with 5-field cron
  + human-friendly (`every Mon at 09:00 America/Bogota`).
  Triggers can call any tool, deliver an inbound event, or
  start a TaskFlow.
- **State machines over TaskFlow** — declarative YAML/JSON of
  states + transitions + actions, compiled to a TaskFlow at
  boot. Better DX than the LLM building flows tool-call by
  tool-call.
- Visual builder in admin UI (Phase 29 follow-up).

Done when an operator wires a recurring weekly digest agent
without writing Rust.

### Phase 54 — Approval workflow library

Destructive ops need an N-of-M approver framework.

- `crates/approvals/` — declarative policies attached to tools
  or actions: `requires_approval: true`, `approvers: […]`,
  `min_approvals: N`.
- Fan-out: admin UI inbox, mobile push (Phase 30), email
  (Phase 48), Slack DM (Phase 22).
- Time-bounded: auto-deny after N hours.
- Audit trail per approval/denial.
- Tiered policies: e.g. amounts < $100 auto-approve,
  $100–1000 needs operator, > $1000 needs operator + compliance.

Done when an agent can request `payment_send($5000)`, two
designated approvers tap-approve from their phones, and the
action proceeds with a full audit trail.

### Phase 55 — Developer experience

Lower friction for contributors and plugin authors.

- `agent dev` — cargo-watch-style auto-restart with
  state-preserving hot-reload where Phase 18 permits.
- VS Code extension `nexo-rs.vscode-nexo`: YAML schema
  validation for `agents.yaml` / `plugins/*.yaml`,
  run-this-skill code lens, jump-to-tool-source.
- `nexo_test_fixtures` crate: bundled fakes (LLM, broker, MCP
  server, channel plugin) for plugin authors writing
  integration tests.
- `cargo make` recipes: `bootstrap`, `smoke`, `lint-all`,
  `audit`.
- Nightly builds on `main` (separate from Phase 27 release
  pipeline) for early adopters.
- Pre-commit hooks bundle.

Done when a new contributor `git clone`s and has a working
dev loop with hot-reload + the VS Code extension showing
problems inline within 5 minutes.

### Phase 56 — RAG over operator docs

Connect agents to the operator's knowledge base.

- `crates/rag/` corpus ingestion:
  - Sources: directory, Notion (Phase 22 follow-up),
    Confluence, Google Drive (Phase 13), GitHub wikis,
    arbitrary URL list.
  - Per-source chunking strategy.
  - Embedding via Phase 47's vector backend.
- `rag_search` tool with relevance threshold + cite-source
  output shape.
- Periodic re-ingest with diff (only re-embed changed chunks).
- Per-agent corpus binding (kate searches docs A, ana docs B).

Done when an operator points at a Notion workspace, runs
`agent rag ingest`, and an agent answers grounded queries
with citations.

### Phase 57 — MCP server marketplace

Plugins (Phase 31) and MCP servers are different ecosystems —
each needs discovery.

- Registry index at
  `https://lordmacu.github.io/nexo-rs/mcp-index.json`.
- `agent mcp install <name>` fetches manifest, prompts for env
  vars, writes to `config/mcp.yaml`.
- Verified vs community tier same as Phase 31.
- Compatibility shim layer for not-quite-spec MCP servers
  (already partly handled in `crates/mcp/`).

Done when an operator can `agent mcp install fetch` and the
LLM has the standard MCP `fetch` tool with no manual config.

### Phase 58 — Streaming UI on companion apps

Make the Flutter app feel as fast as ChatGPT.

- LLM streaming reaches the gateway as `EventFrame` deltas
  (`token`, `tool_call_partial`, `done`).
- Flutter chat surface renders tokens as they arrive; cancel
  button maps to `notifications/cancelled` upstream.
- Mid-stream tool-call rendering ("ana is calling
  `web_search` …").
- Live cost meter per token.
- Voice mode (Phase 23) — frames stream both ways, captions
  appear in chat.

Done when first-token-out latency on the phone is comparable
to ChatGPT and cancel reliably stops mid-tool.

### Phase 59 — Tutorial book + curriculum

Replace the reference-dump docs with an opinionated learning
path.

- `docs/src/book/` chapters:
  1. "Hello, Kate" — one agent on Termux in 10 minutes.
  2. "Add a channel" — WA end-to-end.
  3. "Write a tool" (Rust).
  4. "Write a plugin" (TS / Python via Phase 37).
  5. "Build a workflow" (TaskFlow + cron via Phase 53).
  6. "Deploy" — pick a Phase 40 recipe.
  7. "Operate" — observability + cost + capabilities.
- 90-min workshop format under `docs/src/workshop/`.
- YouTube playlist after the book stabilizes.
- "Ask the docs" widget on the site backed by an MCP search of
  the documentation corpus (eats our own dogfood).

Done when a developer with no Rust experience gets a working
agent on their phone in under an hour by following the book.

### Phase 60 — LLM-stack observability

Beyond Phase 28's runtime observability, the LLM call chain
itself needs eyes.

- Prompt length budget tracker: warn at 80% of model context
  window per turn.
- Cache-hit rate per provider + dashboard.
- Drift alerts: when a prompt's eval score (Phase 51) drops
  vs baseline, notify operator.
- "Why did the agent do that" trace: for any tool call, show
  the prompt that triggered it + the model's reasoning chunk.
- Cost forecasting: project monthly spend at current pace.
- Per-tool error rate, by agent.

Done when an operator opening the admin UI sees at a glance
which agents are healthy / drifting / over budget without
reading individual logs.

### Phase 61 — Self-hosted model serving cluster

Phase 46 covers the single-host local LLM. Operators with real
inference workloads need cluster-grade serving:

- `nexo-inference` deployment recipe wrapping vLLM /
  TensorRT-LLM / TGI behind a stable endpoint.
- Load balancer in front (round-robin, least-loaded by KV-cache
  utilization).
- Model registry: which model lives on which host, with cold/hot
  state, eviction policy.
- Rolling reload — swap a model without dropping in-flight
  requests.
- `agent llm cluster status` CLI showing utilization, queue
  depth, p95 latency per replica.
- Quota / fair-share between agents on a shared cluster.
- Helm chart + Terraform module for k8s deployments.

Done when an operator runs a 2-replica vLLM cluster behind one
endpoint, agents hit it transparently, and a replica reboot
doesn't drop a single request.

### Phase 62 — A/B testing UI bundled

Phase 51 ships the eval harness + shadow traffic primitive.
Phase 62 is the operator-facing UI:

- Admin panel (Phase 29 follow-up) with an "experiments" tab.
- Create experiment: pick agent, define variant prompts /
  models / tool sets, set traffic split (e.g. 90/10), set
  success metric (eval score, conversion proxy, response
  length, manual rating).
- Live results dashboard: streaming metric deltagraph,
  statistical significance hint (chi-square / t-test for
  obvious cases).
- Auto-promote: when variant beats baseline by N% over M
  samples, promote with a single click + revert button.
- Audit trail of every experiment + outcome.

Done when an operator runs a 2-week shadow A/B between two
system prompts and ships the winner with one click.

### Phase 63 — Smart home / IoT

Bring agents into the physical layer.

- `crates/plugins/homeassistant/` — REST + WebSocket subscriber,
  exposes lights, switches, sensors, scenes as agent tools
  (`ha_set_state`, `ha_call_service`, `ha_listen_event`).
- `crates/plugins/mqtt/` — generic MQTT broker plugin so agents
  can publish/subscribe arbitrary topics (sensors, doorbells,
  3D printers, custom firmware).
- `crates/plugins/zigbee2mqtt/` — special-cases Z2M for friendly
  device names.
- Inbound triggers: motion sensor → agent pushes a notification;
  door opened at 3am → wake an agent.
- Outbound capability allowlist per device class (lights yes,
  alarm-system no by default).
- Threat model + sandbox doc — IoT plugins are highest-blast-
  radius outbound (an agent can unlock the front door).

Done when a HomeAssistant install registers, an agent reads
`sun.sun` state, sets a scene by voice, and refuses to operate
a device outside its allowlist.

### Phase 64 — Phone calling (Twilio / SIP / WebRTC)

Phase 23 scaffolds realtime voice. Phase 64 wires real phone
numbers.

- `crates/plugins/twilio/` — voice + SMS via Twilio. Inbound
  call → connects to the realtime-voice pipeline → agent
  answers. SMS works as an additional `plugin.inbound.twilio`
  channel.
- `crates/plugins/sip/` — generic SIP trunk so operators with
  their own PBX (FreeSWITCH, Asterisk, FusionPBX) can route
  calls without Twilio fees.
- WebRTC for in-browser direct calls from the admin / Flutter
  app to an agent.
- DTMF (touch-tone) input as agent events ("press 1 for sales").
- Call recording with consent handling per jurisdiction.
- Per-call cost tracking integrated with Phase 45.

Done when a phone call to a Twilio number connects to an
agent that holds a live voice conversation, and the operator
can also call the same agent from the Flutter app over WebRTC.

### Phase 65 — Calendar bidirectional (Google / Outlook / CalDAV)

Phase 13 ships Google auth. Phase 65 makes calendars a
first-class agent surface.

- `crates/plugins/calendar/` over the Google Calendar API,
  Microsoft Graph, and CalDAV (FastMail, iCloud, NextCloud,
  self-hosted Radicale).
- Inbound: events flow as `plugin.inbound.calendar` so an agent
  can react ("ana, your 3pm meeting starts in 5 min — here's
  the brief").
- Outbound tools: `calendar_create_event`,
  `calendar_invite_attendees`, `calendar_find_free_slot`,
  `calendar_reschedule`, `calendar_decline_with_reason`.
- Free/busy aware scheduling: "find a 30-min slot next week
  with these three people" handles timezone math.
- Recurring events with iCalendar RRULE compliance.
- Out-of-office auto-reply integration (an agent fields email
  when calendar shows OOO).

Done when an agent can be told "schedule lunch with Carlos
next week" and produce a calendar invite that lands in both
inboxes with the right timezone, free/busy-aware.

### Phase 66 — Knowledge graph + entity extraction

Memory today is keyword + vector. Add a structured layer:

- `crates/memory/src/graph.rs` — entity extraction pass over
  every transcript turn (people, orgs, places, dates,
  amounts) using either a small local model (Phase 46) or a
  remote LLM call gated to a budget.
- Relations stored in a side table (`entity_id`, `kind`,
  `aliases`, `mentioned_in_session`, `relation_to`,
  `confidence`). SQLite + JSON, no Neo4j dep.
- New tools: `graph_who_is`, `graph_relations_of`,
  `graph_timeline_for`, `graph_disambiguate`.
- Auto-merge entities by alias, with confidence scoring
  (reduces "Carlos" / "Carlos Rodriguez" / "@carlos" to one
  node).
- Visualization in the admin UI: per-agent entity graph,
  filter by kind, click to see the conversations that
  surfaced each relation.
- Privacy hook: respect Phase 50 `agent forget --user <id>`
  cascading to the graph.

Done when an operator asks "what do you know about Carlos?"
and the agent answers with a structured profile pulled from
the graph plus citations to the conversations that built it.

---

### Phase 67 — Claude Code self-driving agent

Use the `nexo-rs` agent runtime to drive the `claude` CLI as a
sub-process under a verifiable goal. The driver agent reads Claude's
stream, decides allow/deny on every tool call (humano-en-el-loop via
MCP `permission_prompt`), feeds back acceptance failures, and
terminates only when Claude claims "done" AND objective verification
(cargo build/test/clippy/PHASES check) passes. Goal-bound. Memory
across turns so rejected approaches feed forward.

**Stack** (proposed in brainstorm):

#### 67.0 — `AgentHarness` trait + Goal/Attempt/Decision types   ✅

Crate hoja `nexo-driver-types` con el contrato fundacional. Trait
`AgentHarness` (id/label/supports/run_attempt/compact/reset/dispose),
tipos serde `Goal`, `BudgetGuards`, `BudgetUsage`,
`AcceptanceCriterion`, `Decision`, `AttemptOutcome` (Done | NeedsRetry
| Continue | BudgetExhausted | Cancelled | Escalate), wrapper
`CancellationToken` opaco. Mirrors OpenClaw `AgentHarness` shape,
adaptado para Rust + microservicios (todo serializable para NATS).
Sin runtime — solo el contrato sobre el que 67.1+ se montan.

#### 67.1 — `claude_cli` skill (spawn + stream-json + resume)   ✅
#### 67.2 — Session-binding store (SQLite)                      ✅
#### 67.3 — MCP `permission_prompt` in-process                   ✅
#### 67.4 — Driver agent loop + budget guards                    ✅
#### 67.5 — Acceptance evaluator (cargo + custom verifiers)      ✅
#### 67.6 — Git worktree sandboxing + per-turn checkpoint        ✅
#### 67.7 — Memoria semántica de decisiones (vector recall)      ✅
#### 67.8 — Replay-policy (resume tras crash mid-turn)           ✅
#### 67.9 — Compact opportunista                                 ✅
#### 67.10 — Escalación a WhatsApp/Telegram                      ⬜
#### 67.11 — Shadow mode (calibración antes de auto)             ⬜
#### 67.12 — Multi-goal paralelo                                 ⬜
#### 67.13 — Cost dashboard + admin-ui A4 tile                   ⬜

#### 67.A.1 — `nexo-project-tracker` scaffold + PHASES.md parser ✅
#### 67.A.2 — FOLLOWUPS parser + `FsProjectTracker` + watcher    ✅
#### 67.A.3 — `git_log_for_phase` + CircuitBreaker               ✅
#### 67.A.4 — Read tools (`project_status`, `project_phases_list`, `followup_detail`, `git_log_for_phase`) ✅
#### 67.A.5 — `project_tracker.yaml` + capabilities entry        ✅
#### 67.B.1 — `SessionBinding` `origin_channel` + `dispatcher` (schema v2) ✅
#### 67.B.2 — `nexo-agent-registry` scaffold + types + SQLite store ✅
#### 67.B.3 — Cap, FIFO queue, `ArcSwap` snapshot                ✅
#### 67.B.4 — Reattach + per-goal log buffer                     ✅
#### 67.C.1 — Non-blocking `spawn_goal` + `DriverEvent::Progress` ✅
#### 67.C.2 — Pause / resume signal channel                       ✅
#### 67.D.1 — `DispatchPolicy` on agent + per-binding override   ✅
#### 67.D.2 — `DispatchGate` (capability + trust + caps + phase filters) ✅
#### 67.D.3 — `ToolRegistry::apply_dispatch_capability`           ✅
#### 67.E.1 — Tool `program_phase` dispatch                       ✅
#### 67.E.2 — Tool `dispatch_followup`                            ✅
#### 67.F.1 — Hooks core (`notify_origin`/`channel`/`nats_publish`) ✅
#### 67.F.2 — Hook `dispatch_phase` + chain inheritance           ✅
#### 67.F.3 — SQLite hook idempotency                             ✅
#### 67.F.4 — Shell hook exec gated by `allow_shell_hooks`        ✅
#### 67.G.1 — `program_phase_chain` + `program_phase_parallel`   ✅
#### 67.G.2 — `cancel_agent` / `pause_agent` / `resume_agent` / `update_budget` ✅
#### 67.G.3 — Query tools (`list_agents`, `agent_status`, `agent_logs_tail`, `agent_hooks_list`) ✅
#### 67.G.4 — Admin tools (`set_concurrency_cap`, `flush_agent_queue`, `evict_completed`) ✅
#### 67.H.1 — `nexo-driver-tools` CLI subcommands espejo         ✅
#### 67.H.2 — NATS subject inventory + `DispatchTelemetry` trait ✅
#### 67.H.3 — `ToolRegistryCache::get_or_build_with_dispatch` (hot-reload) ✅
#### 67.H.4 — admin-ui tracker / registry / dispatch / hooks tiles ✅
#### 67.H.5 — `architecture/project-tracker.md` mdBook page      ✅
#### 67.H.6 — PHASES + CLAUDE counter + FOLLOWUPS + workspace gate + push ✅

---

### Phase 68 — Local LLM tier (llama.cpp)

Capa-0 transversal del runtime: un host de inferencia local sobre
`llama.cpp` (vía el crate `llama-cpp-2`) que sirve trabajos baratos /
sensibles / offline a cualquier agente, sin reemplazar al LLM cloud
principal. Modelos default: `gemma3-270m` (general) y `bge-small`
(embeddings), quantizados Q4_K_M / IQ4_XS. El target primario es
Termux ARM CPU; desktop CPU/GPU son acelerados pero no obligatorios.

**Por qué llama.cpp y no candle**: 1.5–3× más rápido en ARM CPU por el
hand-tuning NEON, ecosistema GGUF maduro, soporte Termux estándar de
facto. Trade-off: FFI a C++, ABI sync cada 2-3 meses, CI cross-compile
de `libllama` para 4 targets. Se aísla la dep en un crate hoja
(`nexo-llm-local`) detrás de feature flag para que el resto del
workspace no la cargue.

**Modelo intercambiable**: el backend acepta **cualquier GGUF**
compatible con `llama.cpp` — gemma3, llama3.x, qwen2.5, phi3, mistral,
smolLM, deepseek-r1-distill, etc. El operador elige por config:

```yaml
# config/llm.yaml
local:
  models:
    general:
      path: ~/.nexo/models/gemma3-270m-q4_k_m.gguf
      chat_template: gemma         # gemma | llama3 | qwen2 | chatml | auto
      context_size: 8192
      max_concurrent: 4
    classifier:
      path: ~/.nexo/models/qwen2.5-0.5b-q4_k_m.gguf  # mismo trait, otro modelo
      chat_template: qwen2
    embeddings:
      path: ~/.nexo/models/bge-small-q4.gguf
      kind: embedding              # marca que es embedding-only
  jobs:
    pii_redactor:    general
    intent_router:   classifier
    vector_search:   embeddings
```

Nada en el runtime asume gemma3 — el `LocalLlm` trait abstrae el
modelo. Los modelos defaults shipados son una recomendación operativa,
no un hardcode. El operador puede swap a cualquier GGUF que entre en
su presupuesto de RAM y mantener todo el resto del pipeline igual.

**Stack** (propuesto en brainstorm):

#### 68.1 — Crate `nexo-llm-local` scaffold + `LocalLlm` trait      ⬜

Crate hoja con `LocalLlm` trait (espejo reducido de `LlmClient`),
`ModelHandle`, `LocalLlmError` (Load / Oom / Timeout / Cancelled /
BudgetExhausted), feature flags `cpu` (default), `metal`, `cuda`. Sin
backend aún — solo el contrato.

#### 68.2 — `llama-cpp-2` backend + GGUF loader (model-agnostic)    ⬜

Implementación del trait sobre `llama-cpp-2`. Carga **cualquier GGUF**
compatible con `llama.cpp` (gemma3, llama3.x, qwen2.5, phi3, mistral,
smolLM, deepseek-distill, …). Detecta el `chat_template` del header
GGUF; si falta, lee el campo `chat_template` de la config; si tampoco
está, falla con error claro al boot. Expone `generate(prompt,
max_tokens, cancel)` y `embed(texts) -> Vec<Vec<f32>>`. CI
cross-compile para linux-x86_64, linux-aarch64, macos, termux-arm64;
artifact `libllama.a` cacheado. Smoke matrix con 3 modelos
representativos (gemma3-270m, qwen2.5-0.5b, smolLM-135m) para
verificar que el path es genuinamente model-agnostic.

#### 68.3 — `ModelHost` (load / unload / LRU / memory budget)       ⬜

Wrapper que mantiene un mapa `name → Arc<ModelHandle>` con refcount,
load lazy en el primer request, eviction LRU cuando el presupuesto de
RAM (`memory_budget_mb` configurable) se queda corto. `Drop` libera la
memoria del modelo. Métricas: bytes en uso por modelo, evictions, load
duration.

#### 68.4 — Pool + concurrency cap + cancellation                   ⬜

Cada modelo lleva un `tokio::sync::Semaphore` con `max_concurrent`
configurable (CPU inference no escala con threads, hay que limitar).
Requests adicionales encolan con `request_timeout`. Cada inference
acepta un `CancellationToken`; al cancelar, el loop de tokens se corta
en el siguiente token (no a mitad de un kernel call).

#### 68.5 — Integración `nexo-resilience` circuit breaker           ⬜

Wrap del backend con `CircuitBreaker` por modelo. OOM / load fail /
timeout consecutivo abre el breaker N segundos; mientras abierto el
job hace fallback a la ruta cloud. Respeta el patrón ya usado en
`nexo-llm` para Anthropic/MiniMax 5xx.

#### 68.6 — Embeddings backend (`bge-small`) + swap `nexo-memory`   ⬜

Modelo embeddings dedicado (forward pass único, sin generación). Swap
del callsite actual en `nexo-memory::vector` que hoy depende de
embeddings cloud → ahora prefiere local cuando `local.embeddings` está
on. Tests E2E: indexar 100 docs, recall@5 ≥ baseline cloud.

#### 68.7 — PII redactor job (3er backend `redaction.rs`)            ⬜

Tercer modo en `crates/core/src/redaction.rs` (hoy regex + opcional
LLM cloud): `redaction.mode: local`. Usa `gemma3-270m` con prompt
estructurado para devolver spans a redactar. Métrica de precisión vs
modo regex sobre un eval set fijo.

#### 68.8 — Poller pre-filter job                                   ⬜

Builtin extra en `nexo-poller`: `pre_filter` opcional por job que
manda el preview del item al tier-0 con un yes/no prompt. Solo los
"yes" disparan la entrega. Reduce ruido de RSS / Gmail antes de
notificar al agente.

#### 68.9 — Cloud breaker fallback path                             ⬜

Cuando el `CircuitBreaker` del cloud LLM principal está abierto, en
vez de fallar la request el runtime intenta el tier-0 con un prompt
simplificado. Modo "degraded mode" señalizado en la respuesta para
que el agente sepa que el output es de menor calidad.

#### 68.10 — Telemetría + `/healthz/local-llm`                      ⬜

Counters Prometheus: `nexo_local_llm_inference_total{model,job,result}`,
`nexo_local_llm_latency_ms{model,job}`,
`nexo_local_llm_tokens_per_sec{model}`,
`nexo_local_llm_load_total{model,result}`,
`nexo_local_llm_evict_total{model,reason}`,
`nexo_local_llm_memory_bytes{model}`. Endpoint `/healthz/local-llm`
con loaded models, memoria usada, queue depth, OOM 24h, p99.

#### 68.11 — Hot-reload de modelos                                  ⬜

`config/llm.yaml` cambia → `ArcSwap` swap atómico del `ModelHost`
sin restart. Modelos viejos quedan en flight hasta que sus refcount
caen a 0, los nuevos arrancan lazy. Mismo patrón que `RuntimeSnapshot`.

#### 68.12 — Build features + Termux package verify                 ⬜

`cargo build --features cpu` (default) — runs en Termux y servidores
sin GPU. `--features metal` para Mac, `--features cuda` para Linux con
GPU NVIDIA. Verify pipeline: descarga `gemma3-270m-q4_k_m.gguf` de
HuggingFace, corre los 9 jobs en Termux real (Pixel/Snapdragon CI
runner si está disponible, fallback a `qemu-aarch64` si no), reporta
latencias P50/P99 por job.

#### 68.13 — Docs + admin-ui knobs                                  ⬜

Docs: nueva sección `docs/src/llm/local.md` con la matriz de jobs por
device (Termux / desktop CPU / desktop GPU), catálogo de **modelos
recomendados** (gemma3, qwen2.5, llama3.2, phi3, smolLM) con
tamaño/RAM/quality trade-offs, cómo bajar GGUF arbitrarios
(`nexo setup --model <name|url>`), cómo escribir un perfil custom para
un modelo no listado, límites honestos por device. admin-ui: tile A8
con loaded models, queue depth en tiempo real, toggle on/off por job,
presupuesto memoria editable, dropdown para cambiar el GGUF asignado a
cada job sin tocar YAML.

#### 68.14 — Model catalog + auto-download                          ⬜

`nexo setup --download-model <name>` con un catálogo curado
(`crates/setup/data/local-models.toml`) que mapea nombres cortos
(`gemma3-270m`, `qwen2.5-0.5b`, `bge-small`, `llama3.2-1b`, …) a la
URL HuggingFace + sha256 + `chat_template` apropiado. El comando
verifica el sha256 al descargar, resume descargas parciales, y guarda
en `~/.nexo/models/`. Permitir URL custom (`--from-url <url>`) para
modelos fuera del catálogo. Catálogo extensible vía PR — un nuevo
modelo se añade con una entrada toml, sin tocar código.

#### 68.15 — TaskFlow integration patterns + helpers                ⬜

Recipes documentados + helpers en `nexo-llm-local::flow_helpers`
para los 5 patrones donde tier-0 + TaskFlow encajan:

- **Batch indexing**: `Flow.start(goal: "embed N docs")` →
  cada step procesa `batch_size` docs, persiste cursor.
  Reanuda en el doc N+1 tras crash.
- **Chunked summarization**: doc largo → chunks 4k tokens →
  cada step llama `tier_0.summarize(chunk)` → al final
  `merge_summaries`. Crash a la mitad → reanuda.
- **Async rerank**: vector search top-100 → step rerank con
  modelo dedicado → al completar, signal al agente con top-K.
- **Wait-for-model-load**: primer uso de un GGUF grande
  bajando de HF → `WaitCondition::ExternalEvent(
  "local_llm.loaded.<model>")`. Agente puede contestar
  "bajando modelo, te aviso" sin bloquear.
- **Eval harness**: 100 test cases × 2 prompt variants → Flow
  itera, persiste score por case, al final agrega.

Helpers exponen un constructor mínimo (
`flow_helpers::start_batch(items, |chunk| async {...})`) que
oculta el boilerplate de `FlowManager::start_managed` +
`advance` + `finish` para cada llamada al tier-0. Sin los
helpers cada caller reescribe ~80 LOC de plomería.

---

### Phase 69 — Setup wizard agent-centric submenu   ✅

Per-agent submenu inside `nexo setup` that lets the operator pick one
agent and mutate its model, language, channels, and skills from a
single dashboard, without weaving through the service-centric flows.
Reuses every existing channel / LLM / skill flow underneath, so
behaviour stays in lockstep with the rest of the wizard.

- 69.1 — yaml_patch agent-aware helpers (`read_agent_field`,
  `upsert_agent_field`, `remove_agent_field`,
  `append_agent_list_item` (idempotent), `remove_agent_list_item`).
- 69.2 — `agent_wizard.rs` dashboard (`AgentDashboard`,
  `compute_dashboard`, `print_dashboard`) + handlers for Modelo /
  Idioma / Canales / Skills.
- 69.3 — Hub menu wiring under `Configurar agente …`, best-effort
  `try_hot_reload` after every successful mutation, integration tests
  re-parse the mutated YAML through `AgentsConfig`, docs page +
  SUMMARY entry, admin-ui PHASES tech-debt line.

### Phase 78 — Replay-loop visibility   ✅

A goal that returned `Continue` once appeared to stall on turn 0
because the loop had no log between `attempt returned` and the
next spawn — operators couldn't tell whether `replay_policy`
classified, whether `NextTurn` actually fired, or whether the
loop was wedged. Phase 78 closes the gap with structured tracing
plus a synthetic test that pins the Continue → `NextTurn` →
turn N+1 path so a future regression can't silently re-introduce
the stall.

- 78.1 ✅ — `crates/driver-loop/src/orchestrator.rs` emits
  `phase78: spawning attempt`, `phase78: attempt returned`,
  `phase78: replay decision`, plus per-branch markers
  (`FreshSessionRetry — looping`, `NextTurn — looping`) inside
  the replay match. Goal id + turn index on every line so logs
  for one goal can be grepped out of a multi-goal daemon.
- 78.2 ✅ — `crates/driver-loop/tests/orchestrator_replay_continue_test.rs`
  drives the orchestrator with a counter-backed bash mock that
  emits an init-only stream on turn 0 (→ `Continue { reason:
  "stream ended without result event" }`) and a full success
  fixture on turn 1. Asserts `Done` with `total_turns == 2`,
  proving the replay loop advances. New fixture
  `crates/driver-claude/tests/fixtures/continue_no_result.jsonl`.

### Phase 75 — Acceptance autodetect by project type   ✅

`program_phase`'s default acceptance was hardcoded to
`cargo build --workspace` + `cargo test --workspace`, which:

- Wedged every Python / Node / shell goal into a permanent
  `needs_retry` loop because cargo cannot succeed without a
  `Cargo.toml`. The standalone Phase 73 fixture proved the wire
  was clean; the goals still produced no work because acceptance
  always failed.
- Spent 30–60 s per turn rebuilding 200 crates for goals against
  the nexo-rs workspace, even when the diff was a one-line tweak.

Default now branches inside the worktree at acceptance-eval time:
`Cargo.toml` → cargo build + test, `pyproject.toml` /
`setup.py` → `python3 -m pytest -q`, `package.json` →
`npm test --silent`, `CMakeLists.txt` → `cmake build`,
otherwise `true` (auto-pass). Operators that need stricter
checks override per-goal via `acceptance_override` or per-phase
via the markdown `acceptance:` bullets in PHASES.md.

- 75.1 ✅ — `default_acceptance()` returns one shell criterion
  that test-files its way to the right command.
- 75.2 ✅ — 7 unit tests cover Cargo / pyproject / setup.py /
  package.json / CMakeLists.txt / empty-dir fallback / Cargo
  precedence over Python in mixed repos. Each case stubs the
  underlying tool via PATH override to keep the suite hermetic.
- 75.3 ✅ — PHASES.md / CLAUDE.md / admin-ui / docs sync this commit.

### Phase 74 — Claude Code 2.1 MCP conformance   ✅

Phase 73 surfaced eight independent wire-format bugs between our
permission MCP server and Claude Code 2.1. Phase 74 locks them
down with a fixture and adds the schema declarations the new
client expects so the next Claude bump fails loudly instead of
silently dropping every tool from the permission registry.

- 74.1 ✅ — `crates/driver-permission/tests/claude_2_1_conformance.rs`.
  Drives the bin through Claude's exact byte sequence
  (`initialize` with `protocolVersion: 2025-11-25` and
  `capabilities: {roots, elicitation}` → `notifications/initialized`
  → `tools/list` → `tools/call permission_prompt`). 6 tests pin
  every Phase 73 fix: protocol-version echo, fallback for
  unknown versions, `nextCursor` omission, `updatedInput` is a
  required record, `behavior:"deny"` carries `message`, unknown
  tools surface as protocol errors.
- 74.2 ✅ — `McpTool.output_schema: Option<Value>` (MCP
  2025-11-25 SEP-986). `permission_prompt` declares the
  `oneOf[allow, deny]` union so Claude validates against our
  typed shape instead of inferring from responses and drifting.
  Field is `skip_serializing_if = "Option::is_none"` so
  pre-2025-11-25 clients still see the legacy wire.
- 74.3 ✅ — `McpToolResult.structured_content: Option<Value>`.
  `permission_prompt` populates it alongside the legacy text
  content; Claude 2.1 validates the typed object directly,
  killing the "re-parse text as JSON" round-trip that surfaced
  the original `updatedInput` flap. Same skip-if-none discipline
  for compat.

### Phase 73 — Claude Code 2.1 MCP wire fixes   ✅

Cody dispatched goals would burn a full 40-turn budget without
writing a single file. Operator-visible symptom was an empty
worktree plus `notify_origin` never landing. Eight independent
wire-format bugs between the daemon, the permission MCP server,
and the spawned Claude CLI; each one alone made Cody's pipeline
look correct from the outside.

- 73.1 ✅ — `ClaudeCommand` now passes `--verbose` whenever
  `--output-format=stream-json` so Claude does not exit with an
  empty stdout that the driver loop mis-classifies as `Continue`.
- 73.2 ✅ — `ClaudeCommand` always passes `--strict-mcp-config`,
  preventing Claude from merging `--mcp-config` with the user's
  `~/.claude.json` and silently dropping our `nexo-driver`
  server.
- 73.3 ✅ — `write_mcp_config` canonicalises both `bin_path` and
  `socket_path` to absolute form before writing
  `<worktree>/.nexo-mcp.json`, since Claude reads the config with
  `cwd = worktree` and would otherwise resolve `./data/...`
  against a directory that does not exist.
- 73.4 ✅ — MCP server initialise handler echoes the client's
  protocol version when it is one of `2024-11-05 / 2025-03-26 /
  2025-06-18 / 2025-11-25`. Replying with the hardcoded
  `2024-11-05` to a 2.1 client made Claude register the server
  but skip its tools.
- 73.5 ✅ — `tools/list` no longer emits `nextCursor: null`;
  Claude 2.1's Zod validator refused the response shape.
- 73.6 ✅ — `serverInfo.name` = `"nexo-driver-permission"`
  matches the JSON config-key in `.nexo-mcp.json` so Claude's
  tool-namespacing prefix (`mcp__<serverInfo.name>__<tool>`)
  resolves the way `--permission-prompt-tool` looks them up.
- 73.7 ✅ — `permission_prompt_tool` config in
  `config/driver/claude.yaml` updated to
  `mcp__nexo-driver-permission__permission_prompt`.
- 73.8 ✅ — Permission `behavior:"allow"` response now includes
  `updatedInput` as a record (echo of the caller's original
  input when the decider has no override). Without this Claude
  rejected every tool call with `Hook malformed. Returns neither
  valid update nor deny.` and the goal lost the turn silently.

### Phase 72 — Turn-level audit log   ✅

The `LogBuffer` only ever held the last 200 in-memory log lines per
goal — fine for live debugging, useless for "what did this 40-turn
goal actually do over its run?" once the daemon restarted. Phase 72
adds a durable per-turn record on top of `agents.db` and a tool to
read it back from chat.

- 72.1 ✅ — New `TurnLogStore` trait + `SqliteTurnLogStore` in
  `nexo-agent-registry`. Schema:
  `goal_turns(goal_id, turn_index, recorded_at, outcome, decision,
  summary, diff_stat, error, raw_json)` PRIMARY KEY
  `(goal_id, turn_index)` so replays / retries are idempotent.
  Indexes on `recorded_at` and `outcome`. Tail hard-cap at 1000.
- 72.2 ✅ — `EventForwarder.with_turn_log(store)` builder. On every
  `AttemptResult` the forwarder builds a `TurnRecord` (decision
  preview from the last `Decision`, error rendered for
  `NeedsRetry` / `Escalate` / `BudgetExhausted`, full
  `AttemptResult` JSON in `raw_json`) and appends best-effort.
  Append failures log a warn but never block the driver loop.
- 72.3 ✅ — New read tool `agent_turns_tail goal_id=<uuid> [n=N]`
  registered in `READ_TOOL_NAMES` and wired in
  `dispatch_handlers.rs`. Returns a markdown table
  `| turn | outcome | decision | summary | error |` with
  "showing X of Y turn(s)" header. `n` defaults to 20, capped at
  1000. When the turn log isn't enabled, the tool reports
  "set `agent_registry.store` in project_tracker.yaml" instead of
  silently returning empty.
- 72.4 ✅ — Tests:
  4 in `turn_log::tests` (round-trip, idempotent upsert, drop_for_goal
  isolation, n cap),
  2 new in `event_forwarder::tests`
  (`attempt_completed_appends_to_turn_log_when_attached`,
  `build_turn_record_marks_needs_retry_with_failure_summary`),
  3 new in `agent_query::tests` (rendering, empty case,
  `cell` sanitisation).
- 72.5 ✅ — PHASES.md / CLAUDE.md / admin-ui / docs synced this commit.

### Phase 71 — Agent registry persistence + shutdown drain   ✅

Phase 70.4 made gate denials legible; Phase 71 makes the dispatcher
itself crash-resilient. Before this phase the agent registry was
hardcoded to `MemoryAgentRegistryStore` regardless of YAML, every
restart wiped in-flight goals, and SIGTERM gave operators no closure
on goals their chat had asked for.

- 71.1 ✅ — `src/main.rs` honours `agent_registry.store` from
  `project_tracker.yaml`. Resolves env placeholders
  (`${NEXO_AGENT_REGISTRY_DB:-…}`), opens
  `SqliteAgentRegistryStore` with parent-dir creation, falls back to
  memory + warn on open failure. Logs which mode is active so the
  operator can see "agent registry: sqlite-backed" at boot.
- 71.2 ✅ — Boot-time reattach sweep. When the registry is
  sqlite-backed and `reattach_on_boot: true`, every Running row from
  the previous run is flipped to `LostOnRestart`, and any
  `notify_origin` / `notify_channel` hook the operator had attached
  fires once with `[abandoned]` summary so the originating chat sees
  the closure. Resume-as-Running is intentionally OFF (Phase 67.C.1
  territory; respawning a Claude Code subprocess silently is unsafe).
- 71.3 ✅ — `nexo_dispatch_tools::drain_running_goals` helper +
  shutdown wiring. `DispatchToolContext` exposes `hook_dispatcher:
  Option<Arc<dyn HookDispatcher>>`; on SIGTERM the bin walks the
  registry, fires `Cancelled` hooks with `[shutdown]` summary, marks
  rows `LostOnRestart`, all bounded by a 2 s per-hook timeout so a
  stuck publish cannot hold shutdown hostage. Plugin teardown happens
  AFTER the drain so notify_origin actually gets out of the channel.
- 71.4 ✅ — Three unit tests in `shutdown_drain::tests` cover the
  fired-hook path, the non-Running skip path, and the no-matching-hook
  path (where the row still flips to LostOnRestart). Reattach-side
  paths are covered by the existing
  `running_with_resume_off_marks_lost` test in
  `crates/agent-registry/tests/`. Full daemon SIGTERM e2e is left as
  a manual smoke (start daemon, dispatch goal, kill -SIGTERM, watch
  log + chat for `[shutdown]` notify_origin) — automating that
  requires a fixture harness that spins a complete bin under test
  and is deferred under 71.4.x.
- 71.5 ✅ — PHASES.md / CLAUDE.md / admin-ui PHASES / docs/ synced
  in this same commit.

### Phase 70 — Pairing/Dispatch DX cleanup   ✅

Operator-facing polish surfaced after Phase 26/67 landed. The intake
`PairingGate` and the dispatch-side `DispatchGate` share the word
"trusted" but live in different stores; first-run setups silently
swallowed every message because the allowlist was empty; and Cody
was free to invent "tool blocked" replies without ever calling the
tool. This phase closes the loop on each.

- 70.1 ✅ — Cody system prompt: hard rule forbidding hallucinated
  failures. Must call the tool and quote the literal error before
  reporting "blocked / denied / unavailable". Lives in
  `config/agents.d/cody.yaml`.
- 70.2 ✅ — `binding_validate.rs::has_any_override` now recognises
  `dispatch_policy`, `pairing_policy`, `language`, `link_understanding`,
  and `web_search` as overrides, silencing the "binding defines no
  overrides" warn when a binding only narrows dispatch capability.
- 70.3 ✅ — `nexo pair list --all [--include-revoked]` plus
  `PairingStore::list_allow` so seeded senders are visible.
  Operator can confirm `pair seed` actually persisted; doctor + admin-ui
  consume the same view.
- 70.4 ✅ — `[intake]` / `[dispatch]` prefixes on every
  `DispatchDenied` variant + the runtime pairing log lines so the
  origin of a "trusted" denial is unambiguous. `SenderNotTrusted`
  message also points the operator at `program_phase.require_trusted`
  vs the binding-level `pairing.trusted` flag.
- 70.5 ✅ — `nexo pair start` loopback fallback. When the gateway
  is loopback-only, the CLI scans `config/plugins/{telegram,whatsapp}.yaml`
  and prints one ready-to-run `nexo pair seed` per known
  `(channel, account_id)` instead of dumping a bare URL-resolver error.
- 70.6 ✅ — `nexo setup doctor` runs `pairing_check::audit`. Walks
  every binding with `pairing.auto_challenge: true` and reports
  `(channel, account_id)` tuples with no allowlisted senders, suggesting
  the matching `pair seed` command. `run_doctor` is now `async`.
- 70.7 ✅ — `ConfigReloadCoordinator::register_post_hook`. Boot
  registers `PairingGate::flush_cache` as a post-reload hook so
  `nexo reload` (and the file watcher) drop the 30 s decision cache
  and pick up freshly-seeded senders without a daemon restart.
- 70.8 ✅ — PHASES.md / CLAUDE.md / admin-ui / docs sync (this
  phase's progress entry).

### Phase 76 — MCP server hardening   🔄

Today's `crates/mcp/src/server/` is stdio-only (923 LOC) and
single-tenant. The client side (1151 LOC HTTP, sampling, hot-reload,
Phase 73+74 conformance) is production-grade; the server side is the
weak link. To let third-party plugins (`nexo-marketing`, future CRM
/ analytics / billing extensions) build on top of the agent's MCP
surface without touching core, the server needs HTTP+SSE transport,
pluggable auth, multi-tenant isolation, rate-limit, backpressure,
durable sessions, observability, and a builder ergonomic enough that
a new extension is < 50 LOC of bootstrap.

Goal: MCP server becomes a reusable runtime — `nexo-marketing-mcp`
(and any other plugin) consumes it via `McpServerBuilder` and ships
without forking core. Existing stdio path stays at parity (same tests
pass through the new abstraction).

- 76.1 ✅ — HTTP + SSE server transport. New
  `crates/mcp/src/server/http_transport.rs` on `axum 0.7` +
  `tower-http`, plus `http_config.rs`, `http_session.rs`, and
  `parse.rs` (JSON-RPC body defenses). Endpoints: `POST /mcp`,
  `GET /mcp` (SSE), `DELETE /mcp`, `/healthz`, `/readyz`, plus
  optional legacy SSE alias (`GET /sse` + `POST /messages?sessionId=…`)
  behind `enable_legacy_sse: true`. Reuses the 76.2 `Dispatcher`
  verbatim. Defaults: loopback bind, 1 MiB body cap, 30 s
  request timeout, 300 s session idle, 24 h max-lifetime, 1000
  concurrent sessions, 60 rps/IP, 256-event SSE buffer with
  drop-oldest. Boot validates that any non-loopback bind has
  both an `auth_token` and a non-`*` `allow_origins`; otherwise
  refuses to start. Per-call constant-time auth via
  `Authorization: Bearer X` or `Mcp-Auth-Token`. Sessions in a
  DashMap cap'd by `max_sessions` with a 30 s background janitor
  expiring idle / over-lifetime entries. Graceful shutdown
  broadcasts `event: shutdown` to every active SSE consumer
  before tearing the listener down. New `start_http_server`
  re-exported from `nexo_mcp::*`; `src/main.rs::run_mcp_server`
  spins up HTTP and stdio side-by-side when `mcp_server.http.enabled`
  is true. `crates/config/src/types/mcp_server.rs` extended with
  `http: Option<HttpTransportConfigYaml>` + `auth_token_env`
  indirection. 13 adversarial integration tests (body-too-big,
  depth-100, batch reject, missing/unknown session, panic
  recovery, 50-session concurrency, rate-limit, legacy alias,
  idempotent DELETE) and 11 conformance tests porting the
  Phase 12.6/73/74 stdio cases over HTTP, all green. Docs
  page `docs/src/extensions/mcp-server.md` registered in
  `SUMMARY.md`. Done: `cargo test -p nexo-mcp` green
  (228 tests pass: 168 unit + 60 integration); `cargo build
  --workspace` clean.
- 76.2 ✅ — `McpTransport` trait + stdio refactor. Extract
  `read_frame` / `write_frame` abstraction; `StdioTransport` and
  `HttpTransport` both implement; server core depends on the trait
  only. Done: every Phase 73+74 conformance test still passes
  through stdio; HTTP shares 100 % of the protocol path.
- 76.3 ✅ — Pluggable authentication. Trait `McpAuthenticator`
  with `StaticToken`, `BearerJwt` (JWKS fetch + cache + stale-OK
  fallback), `MutualTls::FromHeader`, and `None` (refuses to
  bind non-loopback at boot). Result is a
  `Principal { tenant_id, subject, scopes }` injected into every
  `DispatchContext`. Stdio principal is `Principal::stdio_local()`.
  Boot validation: empty/`none` algorithms refused, HS+asym mix
  refused (algorithm-confusion CVE class), mTLS-from-header
  requires loopback bind. Anti-enumeration: all 401 bodies are
  byte-identical; only `JwksUnreachable` maps to 503.
  Constant-time token comparison via `subtle::ct_eq`. Token
  zeroized on drop via `Zeroizing<String>`. YAML schema lands
  in `nexo-config::types::mcp_server::AuthConfigYaml` with
  back-compat promotion of legacy `auth_token_env`. 20 adversarial
  HTTP tests (`http_auth_test.rs`) + 32 auth-module unit tests.
- 76.4 ✅ — Multi-tenant isolation. `Principal.tenant: TenantId`
  mandatory; flows from auth boundary into `DispatchContext::tenant()`.
  `TenantId::parse` enforces NUL-byte reject, NFKC canonical form,
  percent-decode-and-recheck, `[a-z0-9_-]{1,64}` charset, no
  leading/trailing `_`/`-` (port of
  `claude-code-leak/src/memdir/teamMemPaths.ts:22-64
  sanitizePathKey`). `TenantScoped<T>` trip-wire +
  `CrossTenantError`. `tenant_scoped_path` lexical join with
  absolute/dot-dot fallback. `tenant_scoped_canonicalize` two-pass
  containment (lexical resolve + `realpath` on deepest existing
  ancestor with symlink-loop / dangling / sibling-tenant detection
  — port of
  `claude-code-leak/src/memdir/teamMemPaths.ts:228-256
  validateTeamMemWritePath`); `cfg(unix)` only, Windows port
  deferred. `tenant_db_path` = `<root>/tenants/<tenant>/state.sqlite3`
  (one DB per tenant). YAML schema: `static_token.tenant`,
  `mutual_tls.from_header.cn_to_tenant` (boot-validates each value).
  JWT `tenant_claim` mandatory + parsed through strict validator;
  bad shape → 401 `TenantClaimMissing`. Dotted mTLS CN without
  `cn_to_tenant` → 401 (no silent rewrite — pattern from
  `claude-code-leak/src/services/teamMemorySync/index.ts:163-166`,
  identity claims never silently rewritten). Cross-tenant fixture
  (`multitenant_isolation_test.rs`) boots two HTTP servers with two
  tenants, asserts no marker bleed. 43 new tests across 4 files
  (21 unit `tenant.rs` + 8 symlink containment + 11 HTTP-auth
  tenant flow + 3 cross-tenant integration).
- 76.5 ✅ — Per-principal rate-limiting. Token bucket keyed on
  `(TenantId, ToolName)`, lazy-refill on `check()`, defaults
  100 rps burst 200. Excess returns JSON-RPC `-32099` with
  `data.retry_after_ms` (HTTP stays 200 — the per-IP layer keeps
  emitting HTTP 429 separately; asymmetry intentional and
  documented). `Retry-After` parsing pattern ported from
  `claude-code-leak/src/services/api/withRetry.ts:803-812
  getRetryAfterMs`; the leak is client-side only — wire shape
  is the only direct port. Hard-cap eviction (50 000 buckets,
  drop ~1% LRU on overflow) + background sweeper (60 s
  interval, prunes `last_seen > stale_ttl_secs`). Pattern
  ported from OpenClaw
  `research/src/gateway/control-plane-rate-limit.ts:6-7,101-110`.
  Early-warning `tracing::warn!` at `warn_threshold` utilization
  (default 0.8); concept ported from
  `claude-code-leak/src/services/claudeAiLimits.ts:53-70
  EARLY_WARNING_CONFIGS` (simplified to single fixed threshold).
  Stdio principals bypass entirely (single-tenant by
  construction); `tools/list`, `initialize`, `shutdown` bypass
  too. `DispatchOutcome::Error` extended with optional
  `data: Option<Value>` to carry structured `retry_after_ms`.
  31 new tests (4 token-bucket + 5 config validation + 9 limiter
  unit + 7 dispatcher integration + 1 HTTP concurrent load
  + 5 YAML schema). Sweeper holds `Weak<Self>` so it dies on
  Drop without keeping the limiter alive.
- 76.6 ✅ — Backpressure + concurrency caps. Per-(tenant, tool)
  `tokio::sync::Semaphore` keyed in `DashMap` (mirrors 76.5 shape
  with `TokenBucket` swapped for `Semaphore`). Default
  `max_in_flight: 10`, override per-tool. Bounded `queue_wait_ms`
  (default 5_000) — over wait → JSON-RPC `-32002 concurrent calls
  exceeded` with `data.max_in_flight` + `data.queue_wait_ms_exceeded`.
  Per-call `tokio::time::timeout(timeout_for(tool), handler_fut)`
  wraps every `tools/call`; per-tool override via
  `per_tool[*].timeout_secs`, fallback `default.timeout_secs` then
  `default_timeout_secs` (default 30 s). Hard cap `MAX_TIMEOUT_SECS`
  600 s. Timeout → `-32001 request timeout` with
  `data.timeout_ms`. RAII permits via `OwnedSemaphorePermit` —
  released on success / error / timeout / cancel; verified by load
  test (50 calls × max_in_flight=5 → permits restored). Sweeper
  evicts entries where `available_permits() == max_in_flight`,
  never strands in-flight permits. Hard-cap LRU eviction (default
  50 000). Stdio principals bypass entirely. `tools/list`,
  `initialize`, `shutdown` bypass the cap. `disabled: true` mode
  returns no-op permits from a sentinel semaphore so caller code
  path is uniform. Reference patterns: RAII acquire from in-tree
  `crates/mcp/src/client.rs:873-899`; cancellation propagation
  pattern from `claude-code-leak/src/Task.ts:39` + `src/services/
  tools/toolExecution.ts:415-416` (the leak itself does NOT
  implement server-side caps — only the cancel idea is portable);
  unbounded-queue anti-pattern flagged in OpenClaw
  `research/src/acp/control-plane/session-actor-queue.ts:6-37`,
  explicitly rejected. 36 new tests across 3 files (3 entry +
  13 config + 11 limiter unit + 8 dispatcher integration +
  1 HTTP load).
- 76.7 ✅ — Server-side notifications + streaming. Closes the
  `notifications/tools/list_changed`, `resources/list_changed`,
  `resources/updated` loops on the server (Phase 12.8 client-side
  pre-existed) via new `HttpServerHandle::notify_tools_list_changed`,
  `notify_resources_list_changed`, and `notify_resource_updated`
  methods. Adds `notifications/progress` via a new trait method
  `McpServerHandler::call_tool_streaming(name, args, ProgressReporter)`
  with a default impl that delegates to `call_tool` (non-breaking).
  `ProgressReporter` is `Clone`, non-blocking, and drop-oldest on
  broadcast overflow; coalesces with a 20 ms gate per reporter so
  a tight-loop `report()` doesn't storm the wire (last call wins
  on flush). `noop()` reporter is allocation-free for callers
  without a `progressToken`. `resources/subscribe` /
  `resources/unsubscribe` arms in the dispatcher persist per-session
  URI subscriptions in a `DashSet<String>` on `HttpSession`;
  `notify_resource_updated` only fans out to subscribed sessions.
  `DispatchContext` extended with `progress_token: Option<Value>`
  and `session_sink: Option<broadcast::Sender<SessionEvent>>` —
  HTTP transport extracts the token from `params._meta.progressToken`
  (strict MCP 2025-11-25) and the sink from the session's
  `notif_tx`. Stdio principals receive `session_sink: None` →
  reporter is automatically noop. `SessionEvent` promoted to
  `pub` + `#[non_exhaustive]`. Capability advertisement now
  default-enables `tools.listChanged`, `resources.listChanged`,
  `resources.subscribe` so clients don't need to probe. Reference
  patterns: client-side consumption shape from
  `claude-code-leak/src/services/mcp/useManageMCPConnections.ts:618-664`
  (the leak does NOT implement server-side notifications — it
  consumes them from upstream MCP servers). 11 new tests across
  3 files (7 progress unit + 1 dispatcher integration progress
  storm + 3 progress e2e). Wire shape compact JSON-RPC notification
  routed through existing `SessionEvent::Message` per-session
  broadcast — no new variant needed (Phase 76.1 already shipped
  the right primitive).
- 76.8 ✅ — Durable sessions + SSE `Last-Event-ID` reconnect. New
  module `crates/mcp/src/server/event_store/` (4 files,
  ~1100 LOC) shipping a `SessionEventStore` trait +
  `MemorySessionEventStore` (tests-only) +
  `SqliteSessionEventStore` (prod, WAL + synchronous=NORMAL,
  `INSERT OR IGNORE` idempotent on `(session_id, seq)`,
  WITHOUT ROWID PK, sibling `mcp_session_subscriptions` table
  with replace-set semantics). 18 unit tests (5 config + 5
  in-mem + 8 sqlite). `HttpSession.next_seq: AtomicU64` (starts
  at 1; seq 0 reserved for "no events yet"). New variant
  `SessionEvent::IndexedMessage { seq, body }` — non-breaking
  on the `non_exhaustive` enum; `progress.rs` keeps emitting
  `Message(_)` because per-call progress is by-design ephemeral.
  `HttpSessionManager::with_event_store()` constructor +
  `emit_to(session, body)` assigns seq, persists best-effort
  via `tokio::spawn`, broadcasts `IndexedMessage`. Cap
  enforcement: every 1000th emit triggers
  `purge_oldest_for_session(keep=max_events_per_session)`.
  `broadcast_to_all` + `notify_resource_updated` route through
  `emit_to` so `notifications/tools/list_changed` +
  `resources/list_changed` + `resources/updated` all replay.
  `SessionLookup::subscribe`/`unsubscribe` impls now persist
  the URI delta via `put_subscriptions(...)` so a reconnecting
  client's subscription set survives. SSE handler reads
  `Last-Event-ID` header — `Option<u64>`: absent → live only,
  present (any value) → drains
  `manager.replay(session_id, min_seq)` capped at
  `max_replay_batch` before transitioning to live. Each replay
  + live `IndexedMessage` carries the SSE `id: <seq>` line —
  matches `claude-code-leak/src/cli/transports/SSETransport.ts:159-266`.
  Unknown session → HTTP 404 + JSON-RPC
  `{"error":{"code":-32001,"message":"Session not found"}}` —
  matches `claude-code-leak/src/services/mcp/client.ts:189-206`.
  YAML schema `session_event_store` block on
  `HttpTransportConfigYaml` (5 fields with defaults — enabled,
  db_path, max_events_per_session=10_000,
  max_replay_batch=1_000 with 10_000 ceiling,
  purge_interval_secs=60). `yaml_session_event_store_to_runtime`
  mapper in `src/main.rs::start_http_transport`. `start_http_server`
  opens the SQLite store + injects into `HttpSessionManager` +
  spawns a periodic purge worker that calls
  `purge_older_than(now - session_max_lifetime_ms)` every
  `purge_interval_secs`, stops on parent shutdown. 4 e2e tests
  in `crates/mcp/tests/http_session_resume_test.rs`: unknown
  session → 404 + -32001, Last-Event-ID absent → live with seq
  labels, Last-Event-ID=N → replays only seq > N,
  max_replay_batch caps the initial drain. **Out of scope for
  76.8.b**: full session reattach across daemon restart
  (rehydrating `HttpSession` entire — events + subs survive,
  but in-mem session is gone, client re-`initialize`s; matches
  the leak's `isMcpSessionExpiredError` permanent-failure
  contract). **Out of scope for 76.14**: read-side ops CLI
  `nexo mcp-server tail-events`.
- 76.9 ✅ — `McpServerBuilder` ergonomic API (core; proc-macro
  follow-up). New `crates/mcp/src/server/builder.rs` (~440 LOC):
  * `Tool` async trait with typed `Args: DeserializeOwned +
    JsonSchema` and `Output: Serialize`. Default `deferred(): false`,
    `search_hint(): None` (Phase 79.2 surface ready).
  * `ToolCtx<'a>` borrowed-fields ctx (tenant, correlation_id,
    session_id, progress, cancel) + `ToolCtxOwned` for the boxed
    handler future.
  * `McpServerBuilder::new(name, version).tool(impl).build_handler()`
    returns `BuiltHandler` which implements `McpServerHandler`.
  * Schema derived **once at registration** via
    `schemars::schema_for!(T::Args)` and cached as `Value` —
    eliminates the hand-rolled-schema drift in
    `crates/core/src/agent/web_search_tool.rs:26-42`. Hard cap
    `MAX_SCHEMA_BYTES = 64 KB` panics at registration on cyclic /
    pathological types so the operator notices.
  * `list_tools()` returns deterministic alphabetical order so
    schema diff stays byte-stable for clients.
  * Duplicate-name registration warns + overwrites (mirrors
    Phase 11.5 `ToolRegistry::register` semantics).
  * `examples/hello_mcp.rs` ships a stdio MCP server with one
    typed tool in **60 LOC of operator code** vs ~120 LOC pre-76.9
    (ToolHandler trait + JSON literal + manual register).
  * Reference patterns: `claude-code-leak/src/Tool.ts:362-695`
    one-tool-per-struct shape; `:783-792 buildTool(def)` defaults
    helper; `src/tools/WebSearchTool/WebSearchTool.ts:25-41`
    lazy-schema concept (we cache once at registration).
  * 6 unit tests (registers + lists, round-trip typed args,
    rejects invalid args, rejects unknown name, deterministic
    list order, duplicate overwrites with warn).
  Deferred to follow-up (76.9.b): `#[mcp_tool]` proc-macro in
  `crates/mcp-macro/` to drop the boilerplate from ~40 LOC per
  tool to ~10. Trait foundation enough for the marketing plugin
  to start; macro is sugar.
- 76.10 ✅ — Server-side observability + health. New module
  `crates/mcp/src/server/telemetry.rs` (~470 LOC) emits hand-rolled
  Prometheus text via `LazyLock<DashMap<Key, AtomicU64>>` module
  globals (in-tree pattern from `crates/web-search/src/telemetry.rs`,
  `crates/llm/src/telemetry.rs`). Metrics:
  `mcp_requests_total{tenant,tool,outcome}`,
  `mcp_request_duration_seconds{tenant,tool}` (8 buckets:
  50/100/250/500/1k/2.5k/5k/10k ms),
  `mcp_in_flight{tenant,tool}` (signed gauge via RAII
  `InFlightGuard` — drops on success/error/timeout/cancel/panic),
  `mcp_rate_limit_hits_total{tenant,tool}`,
  `mcp_timeouts_total{tenant,tool}`,
  `mcp_concurrency_rejections_total{tenant,tool}`,
  `mcp_progress_notifications_total{outcome=ok|drop}`. `Outcome`
  enum bounded set (ok/error/cancelled/timeout/rate_limited/
  denied/panicked) — reusable by 76.11 audit. **Cardinality
  discipline**: tool-name allowlist capped at 256 distinct names;
  beyond → "other". Pattern ported from
  `claude-code-leak/src/services/analytics/datadog.ts:195-217`
  (`mcp__*` collapsed to `'mcp'`). Tenant labels bounded by
  `TenantId::parse` (`[a-z0-9_-]{1,64}`). `DispatchContext`
  extended with `correlation_id: Option<String>`; HTTP transport
  extracts `X-Request-ID` (or generates UUIDv4), echoes back in
  response, logged on every dispatch span. Client-supplied values
  >128 chars replaced with fresh UUIDv4 (don't trust unbounded
  headers). Render wired into the existing
  `/metrics` aggregator at `src/main.rs:8059` alongside
  `nexo_mcp::telemetry::render_prometheus()` (session-lifecycle).
  11 unit tests (RAII drop on panic unwind, bucket-cumulative
  semantics, cardinality cap, every metric family has HELP+TYPE).
  Tests serialised via `serial_test = "3"` because module-globals
  are shared. **Note**: `/healthz` + `/readyz` and dependency
  caching landed as part of this phase; `/readyz` 503 vs 200 +
  structured JSON is in `src/main.rs`.
- 76.11 ✅ — Per-call audit log core. New module
  `crates/mcp/src/server/audit_log/` with: `AuditRow` (18-field
  schema mirroring SQLite columns: call_id, request_id, session_id,
  tenant, subject, auth_method, method, tool_name, args_hash,
  args_size_bytes, started_at_ms, completed_at_ms, duration_ms,
  outcome, error_code, error_message, result_size_bytes,
  retry_after_ms), `AuditFilter`, `AuditError`, `AuditLogStore`
  trait + `MemoryAuditLogStore` (in-memory, tests-only),
  `AuditLogConfig` (validate + per-tool redact override + 1 MiB
  args-hash cap), `AuditWriter` (`tokio::mpsc` bounded 4096,
  batched worker every 50 ms or 50 rows, drop-oldest with
  `tracing::error!` at power-of-2 thresholds, `drain(timeout)`
  for SIGTERM). `Outcome` enum re-exported from 76.10 telemetry
  (single source of truth: ok/error/cancelled/timeout/
  rate_limited/denied/panicked). `Dispatcher::with_full_stack`
  constructor takes optional `Arc<AuditWriter>`; `do_dispatch`
  for `tools/call` emits one `AuditRow` per outcome with
  truncated error_message (512 char cap) + retry_after_ms
  extraction from JSON-RPC error data. Anti-pattern flagged:
  the leak's `claude-code-leak/src/services/analytics/
  firstPartyEventLogger.ts:57-85 shouldSampleEvent` drops
  events probabilistically; **76.11 logs every dispatch at
  100% — sampling forbidden for compliance**. 26 new tests
  (3 types + 8 store + 7 config + 4 writer + 3 e2e + 1 unused).
  **Production wire-up shipped**: `SqliteAuditLogStore`
  (`crates/mcp/src/server/audit_log/sqlite_store.rs`, ~440 LOC,
  WAL + synchronous=NORMAL + 3 indexes + idempotent INSERT OR
  REPLACE in transaction, mirrors Phase 72 `turn_log.rs`, 9
  passing unit tests including round-trip + filter + idempotent
  upsert + retention purge). `HttpTransportConfig.audit_log:
  Option<AuditLogConfig>` field; `HttpTransportConfigYaml.audit_log:
  Option<AuditLogYaml>` mirror in `crates/config/src/types/mcp_server.rs`
  with `deny_unknown_fields` + 9 default fns. `yaml_audit_log_to_runtime`
  mapper in `src/main.rs::start_http_transport`. `start_http_server`
  in `crates/mcp/src/server/http_transport.rs` opens
  `SqliteAuditLogStore::open(db_path)` + spawns `AuditWriter` when
  `audit_log.enabled = true`, switches dispatcher constructor from
  `with_rate_concurrency_and_sessions` to `with_full_stack`. SIGTERM
  graceful-shutdown closure calls `audit_writer.drain(Duration::from_secs(5))`
  before axum tears down the listener — pending rows flush
  synchronously inside the Phase 71 5 s shutdown budget. **Deferred
  to follow-ups**: `args_hash` computation in dispatcher hot path
  (currently emits `args_hash: None` + `args_size_bytes: Some(N)`),
  `mcp_audit_tail` read tool (read-side surface for ops + Phase
  76.14 CLI).
- 76.12 ✅ — Conformance + fuzz suite. `tests/parse_fuzz_test.rs`
  (5 proptest cases, 7500 generated inputs — arbitrary bytes/strings/
  methods/depths/batches — asserts `parse_jsonrpc_frame` never panics).
  `tests/stdio_conformance_test.rs` (11 spec MCP 2025-11-25 fixtures
  via stdio transport, transport-parity twin of
  `http_conformance_test.rs`). `tests/load_smoke_test.rs` (50 sessions
  × 200 requests = 10 000 calls, p99 gate < 500 ms, `#[ignore]`).
  `ConformanceHandler` extracted to `tests/conformance_shared/mod.rs`.
  Feature flag `server-conformance` in `Cargo.toml` gates all three
  new files. Done: `cargo test -p nexo-mcp --features server-conformance`
  green (508 tests, 0 failures).
- 76.13 ✅ — TLS + reverse-proxy guidance. Optional `rustls` behind
  feature `server-tls` for direct termination; docs recommending
  nginx/caddy/Traefik in front for prod; mTLS recipe for
  in-VPC nexo-core ↔ extension-mcp. Done: example nginx + caddy
  configs in `docs/src/extensions/mcp-server.md`.
- 76.14 ✅ — CLI ops `nexo mcp-server`. Subcommands: `inspect <url>`
  (lists tools/resources of any reachable server), `bench <url>
  --tool X --rps N` (load test), `tail-audit <db>` (reads
  `mcp_call_log`). Smoke entry in `scripts/release-check.sh`.
  Done: subcommands present in CLI, smoke green.
- 76.15 ✅ — Docs + extension template shipped. New skeleton
  `extensions/template-mcp-server/` (workspace member,
  ~250 LOC: `Cargo.toml` with `nexo-mcp` path dep + crates.io
  swap doc, `plugin.toml` extension manifest, `src/main.rs`
  always-stdio + opt-in HTTP via `MCP_TEMPLATE_HTTP_BIND` /
  `MCP_TEMPLATE_HTTP_TOKEN` env, `src/tools.rs` typed `Echo`
  tool using `McpServerBuilder` + `JsonSchema` derive +
  `Tool` async trait, `config.example.yaml` documenting every
  `mcp_server.http` block (auth/CORS/sessions/per-IP +
  per-principal rate-limit + per-principal concurrency +
  audit_log + session_event_store, all commented for
  copy-paste), `README.md` quickstart + 5-step fork +
  production checklist + troubleshooting). Stdio smoke
  end-to-end green: `initialize` → `tools/list` (echoes
  derived JSON Schema with `text` field) → `tools/call`
  (returns `structuredContent: {echoed: ...}`) →
  `shutdown`. New docs chapter
  `docs/src/extensions/mcp-server-extension.md`
  registered in `docs/src/SUMMARY.md` — developer-facing
  walk-through (when to build vs ship as built-in tool;
  5-step fork; SendEmail tool sample; child-process vs
  long-lived HTTP wiring; production checklist mapping every
  knob to its phase). `mdbook build docs` clean.
  `cargo build -p template-mcp-server` green. **Out of
  scope** (deferred): `notifications/progress` sample,
  `notifications/tools/list_changed` sample, resources +
  prompts surface, custom error types, `#[mcp_tool]`
  proc-macro (Phase 76.9 follow-up).

- 76.16 ✅ — `expose_tools` whitelist for MCP server. Adds
  `expose_tools: Vec<String>` and `allow_config_tool: bool` to
  `McpServerConfig`. The `run_mcp_server` function loops over the
  list and registers 7 Phase 79 tools into the `ToolRegistry`
  (EnterPlanMode, ExitPlanMode, ToolSearch, TodoWrite,
  SyntheticOutput, NotebookEdit, RemoteTrigger). `Config` and `Lsp`
  are explicitly gated with `tracing::warn!` and deferred (see
  FOLLOWUPS.md). 5 integration tests in
  `crates/core/tests/expose_tools_bridge_test.rs` verify filtering,
  allowlist, proxy-tool hiding, and blocked call_tool error.
  Docs updated in `docs/src/extensions/mcp-server.md`.
  `cargo build --workspace` + all 5 tests green.

**Acceptance for the whole phase:** stdio path keeps every Phase
73+74 test green; HTTP path passes the same conformance suite plus
multi-tenant, rate-limit, backpressure, sessions, audit, and the
smoke `examples/marketing_mcp_skeleton.rs` boots an authenticated
server in under 50 LOC of plugin code. After 76.x lands the
`nexo-marketing` extension can be built without a single change to
`crates/mcp/`.

**Critical path for the marketing extension:** 76.1 → 76.2 → 76.3
→ 76.4 → 76.5 → 76.9. Rest hardens production but does not gate
the first marketing MVP.

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

### Phase 15.9 — Anthropic OAuth Claude-Code request shape   ✅

**Goal:** Unblock Opus 4.x / Sonnet 4.x for users on Claude
Pro / Max plans by mirroring the request envelope Claude Code itself
sends. Before this phase only Haiku passed Bearer-auth; Opus / Sonnet
failed with a 4xx that the runtime collapsed into a generic "no
quota" error.

Done criteria:

- [x] `AnthropicAuth::subscription_betas() -> &'static [&'static str]`
  returns `["claude-code-20250219", "oauth-2025-04-20",
  "fine-grained-tool-streaming-2025-05-14"]` for Bearer variants.
- [x] `AuthHeaders` grows `extra: Vec<(&'static str, String)>` carrying
  `User-Agent: claude-cli/<version>`, `x-app: cli`,
  `anthropic-dangerous-direct-browser-access: true` only on Bearer
  paths.
- [x] `build_body` prepends the canonical `"You are Claude Code,
  Anthropic's official CLI for Claude."` system block at index 0 when
  the auth variant is a subscription. Legacy string-shaped `system` is
  promoted to a 2-element array; existing arrays get the spoof
  inserted at position 0.
- [x] `merge_beta_headers` accepts subscription betas, dedupes against
  existing + cache betas, preserves first-occurrence order.
- [x] `crates/llm/src/text_sanitize.rs::sanitize_payload_text` ports
  OpenClaw's lone-surrogate stripper as a defensive parity guard
  (zero-copy `Cow` on valid UTF-8).
- [x] `classify_response` logs the truncated body via
  `tracing::warn!(target = "anthropic", …)` before collapsing
  generic 4xxs into `LlmError::Other` so the next "no quota" surprise
  leaves a diagnosable reason in logs.
- [x] `NEXO_CLAUDE_CLI_VERSION` env var overrides the User-Agent
  version stamp without a release.
- [x] Tests: `oauth_request_shape_prepends_claude_code_spoof`,
  `api_key_request_shape_unchanged_no_spoof`,
  `build_body_promotes_string_system_when_subscription`,
  `build_body_creates_system_array_when_subscription_and_no_user_system`,
  `merge_beta_headers_with_subscription_dedupes_oauth_beta`,
  `subscription_betas_only_for_bearer`,
  `setup_token_headers_bearer_with_beta` (extended for `extra`),
  `user_agent_resolves_default_override_and_blank`.
- [x] Docs: `docs/src/llm/anthropic.md` documents the OAuth
  subscription request shape and `NEXO_CLAUDE_CLI_VERSION` override.
- [x] API-key path unchanged — none of the spoof headers or the
  system block leak into static `x-api-key` requests.

Reference: `research/src/agents/anthropic-transport-stream.ts:558-641`.

### Phase 77 — Claude Code parity sweep (claude-code-leak)   ⬜

After auditing the leaked `claude-code-leak/src/` tree
(2026-03-31, ~1,900 files), several patterns from Anthropic's
production Claude Code CLI are missing from nexo-rs and would
materially improve robustness, memory hygiene, safety, and
operator UX. Phase 77 ports the high-value subset — compact
multi-tier, memdir hygiene, bash semantic guards, post-turn
memory extraction, prompt-cache-break detection, plus four
skills (`loop`, `stuck`, `simplify`, `verify`), an
`AskUserQuestion` mid-turn elicitation tool, a versioned
schema migrations system, and the coordinator/worker mode
split. Voice/STT, Ink UI, IDE bridge, and GrowthBook
analytics are explicitly out of scope (different tech stack
or proprietary).

Cross-cutting rule for Phase 77:
- Every `77.x` subphase must be designed as multi-model +
  multi-provider by default. Provider-specific logic is allowed
  only as additive enrichment and must not be the sole path.

Goal: long-running conversations stop hitting the context
ceiling (microcompact + autocompact + cache-break detection),
shared memories stop leaking secrets (memdir scanner), bash
destructive ops stop slipping past capability gates
(semantic guard), and skill authors get four production-grade
patterns ready to copy.

References:
- `claude-code-leak/src/services/compact/` (3.5 K LOC, 11 files)
- `claude-code-leak/src/memdir/` (8 files, 1.6 K LOC)
- `claude-code-leak/src/services/extractMemories/`
- `claude-code-leak/src/tools/BashTool/{bashSecurity,bashPermissions,sedValidation,pathValidation,shouldUseSandbox,destructiveCommandWarning}.ts`
- `claude-code-leak/src/tools/AskUserQuestionTool/`
- `claude-code-leak/src/skills/bundled/{loop,stuck,simplify,verify}.ts`
- `claude-code-leak/src/coordinator/coordinatorMode.ts`
- `claude-code-leak/src/migrations/`
- `claude-code-leak/src/services/api/promptCacheBreakDetection.ts`

#### 77.1 — microCompact (inline tool-result compression)   ✅

In `crates/driver-loop/` (or `crates/core/agent/`) add a per-turn
hook that, when a single tool result exceeds a configurable
byte threshold (default 16 KiB), summarises it via a cheap LLM
call (Haiku / local tier-0) and replaces the body in-place
while keeping the tool_use_id/tool_result_id pair intact for
the model's tool-loop bookkeeping. Reference:
`services/compact/microCompact.ts` (530 LOC).

Done when:
- Threshold + summariser provider configurable per binding
  (`compact.micro.threshold_bytes`, `compact.micro.provider`).
- Original tool result archived to the turn-log audit trail
  (Phase 72) so post-mortem replay still has the full body.
- Unit test: 1 MiB grep result is compressed to ≤ 2 KiB and
  the next turn still references the same `tool_use_id`.

Shipped in nexo-rs as pre-send `ChatMessage::Tool` compaction:
the canonical in-memory `messages` vector keeps the full
tool result for local replay/audit, while the request clone replaces
oversized compactable tool results with Claude Code's stable marker
`[Old tool result content cleared]` by default. If
`context_optimization.compaction.micro.provider` is set, the already
wired compactor LLM path summarizes that single result instead.
The implementation mirrors the current leak's
`claude-code-leak/src/services/compact/microCompact.ts`
contract: compact only known high-volume tools, preserve
`tool_use_id`/`tool_result` correlation, and operate immediately
before the provider request.

#### 77.2 — autoCompact (token + time triggered)   ✅

Loop-level autocompact that fires when the running token
estimate crosses `compact.auto.token_pct` (default 80 % of the
model context window) OR the conversation has been alive for
`compact.auto.max_age_minutes` (default 120). Compresses the
oldest non-pinned turns into a single summary block. Reference:
`services/compact/autoCompact.ts` (351 LOC) +
`timeBasedMCConfig.ts`.

Shipped:
- `AutoCompactionConfig` in `nexo_config::types::llm` with five
  fields: `token_pct`, `max_age_minutes`, `buffer_tokens`,
  `min_turns_between`, `max_consecutive_failures` (all with serde
  defaults). `CompactionConfig::auto: Option<AutoCompactionConfig>`
  — age trigger disabled when `None`.
- `CompactPolicy` trait + `CompactContext` + `CompactTrigger` enum
  + `DefaultCompactPolicy` + `AutoCompactBreaker` moved to
  `nexo_driver_types::compact_policy` (shared by driver-loop and
  core agent without cycle).
- `DefaultCompactPolicy::classify()` checks both triggers: token
  pressure first (uses `auto.token_pct` when present, else legacy
  `threshold`), age second (gated on `auto` being `Some` and
  `max_age_minutes > 0`). Both respect `min_turns_between`
  anti-storm guard.
- `AutoCompactBreaker` tracks `consecutive_failures` +
  `last_compact_turn`. Trips after `max_consecutive_failures`,
  resets on success.
- `CompactionRuntime` (core agent) extended with all `auto_*`
  fields + `AtomicU32`/`Mutex<Option<u32>>` for runtime breaker
  state. Age check in pre-flight uses `Session.created_at`.
- Driver-loop `events.rs`: `CompactRequested` extended with
  `before_tokens`, `age_minutes`, `trigger: CompactTrigger`.
  `CompactCompleted { goal_id, turn_index, after_tokens }` added
  with NATS subject `agent.driver.compact.completed`.
- Driver-loop `orchestrator.rs`: `Mutex<AutoCompactBreaker>`,
  `auto_config`, `session_age_minutes` from `started.elapsed()`.
  Circuit breaker checked before classify, failures recorded on
  compact turn errors, successes reset breaker.
- Driver-loop config `CompactPolicyConfig.auto: Option<AutoCompactionConfig>`.
- `nexo_driver.rs` bin wires auto config to builder.
- Old `crates/driver-loop/src/compact.rs` removed; types live in
  `nexo-driver-types` (leaf crate, no cycles).
- 21 unit tests in `nexo-driver-types::compact_policy` + all
  existing driver-loop orchestrator/replay/sleep tests pass.
- Docs: `docs/src/ops/compact-tiers.md` updated with YAML
  examples, event subjects, trigger descriptions, guards.

Deferred: 50-turn synthetic-goal integration test (Step 8) —
requires full Claude subprocess harness; unit coverage is
comprehensive.

#### 77.3 — sessionMemoryCompact + postCompactCleanup   ✅

After 77.1+77.2 fire, persist the compacted summary into the
session's long-term memory entry (Phase 5.3) and clean
references to the now-archived tool_use ids from the prompt
cache. Reference: `services/compact/sessionMemoryCompact.ts`
(630 LOC), `postCompactCleanup.ts`.

Shipped:
- `CompactSummaryStore` trait + `CompactSummary` struct in
  `nexo-driver-types::compact_policy`
- `SqliteCompactSummaryStore` — persists via `LongTermMemory::remember()`
  with FTS5-searchable goal_id in content; `load()` retrieves most
  recent; `NoopCompactSummaryStore` for tests
- `DriverOrchestrator` gains `compact_store` field + builder setter;
  on compact success extracts `result.final_text`, builds
  `CompactSummary`, calls `store()`, emits `CompactSummaryStored`;
  on resume (goal start) calls `load()` and injects
  `compact_summary` into `next_extras`
- `CompactSummaryStored` event with NATS subject
  `agent.driver.compact.summary_stored`
- `PostCompactCleanup` placeholder module (no-op, wired after
  persistence; real cleanup lands in 77.5+)
- `TranscriptLine::CompactBoundary` variant in
  `core/src/agent/transcripts.rs`
- `SmCompactConfig` field in `CompactPolicyConfig` (YAML:
  `compact_policy.sm_compact`)
- 3 Noop store unit tests + docs in `compact-tiers.md`

Done when:
- A resumed session can read the compacted summary from
  `crates/memory` long-term store and re-prime the model
  without re-running the elided turns. ✅ (wired end-to-end;
  SQLite roundtrip test deferred — needs `LongTermMemory` file-path setup)
- `agent_turns_tail` tool (Phase 72) flags compacted turns
  with a `compacted=true` column. (deferred — Phase 72 was
  shipped before 77.3; the CompactBoundary transcript line
  provides the marker for future integration)


#### 77.4 — promptCacheBreakDetection   ✅

In shared runtime/LLM layers (`crates/core/src/agent/llm_behavior.rs`
+ provider adapters), after every API response
parse `usage.cache_read_input_tokens` and
`usage.cache_creation_input_tokens` against the previous turn.
When the read drops by > 50 % unexpectedly, log
`llm.cache_break` with the suspected breaker (provider swap,
model swap, system prompt mutation). Provider-specific enrichments
(for example Anthropic beta-header drift via `anthropic.cache_break`)
are optional additive signals. This lets an operator root-cause
cache misses without staring at raw usage rows.
Reference: `services/api/promptCacheBreakDetection.ts`.

Done when:
- Generic `llm.cache_break` detection runs for any provider/model;
  Anthropic additionally emits `anthropic.cache_break` with
  beta-header drift hints.
- Unit tests: synthetic cache-hit run vs. cache-break run
  produce the expected log lines.
- Docs: `docs/src/llm/anthropic.md` documents the diagnostic.

#### 77.5 — extractMemories (post-turn LLM extraction)   ✅

Shipped:
- `ExtractMemoriesConfig` in `crates/driver-types/src/compact_policy.rs`
  — `enabled` (default false), `turns_throttle`, `max_turns`,
  `max_consecutive_failures`.
- `ExtractMemories` struct in `crates/driver-loop/src/extract_memories.rs`
  — state machine (`ExtractMemoriesState`), gate checks (disabled /
  throttled / in-progress / circuit-breaker / main-agent-wrote),
  coalescing, path sandbox, MEMORY.md index management.
- `scan_memory_manifest()` — reads `memory/*.md` YAML frontmatter
  (`name`, `description`, `type`) for pre-injection into extraction
  prompt.
- `has_memory_writes_in_text()` — heuristic to detect when the main
  agent already wrote to the memory dir this turn (skip extraction).
- `extract_memories_prompt.rs` — full port of Claude Code's
  `services/extractMemories/prompts.ts` + `memdir/memoryTypes.ts`:
  4-type taxonomy (user/feedback/project/reference), WHAT NOT TO
  SAVE exclusion list, markdown frontmatter template, 2-step save
  process.
- `ExtractMemoriesLlm` trait — narrow `chat()` interface decoupled
  from `nexo_llm::LlmClient`; `NoopExtractMemoriesLlm` for tests.
- Two `DriverEvent` variants: `ExtractMemoriesCompleted` +
  `ExtractMemoriesSkipped { reason: ExtractSkipReason }`.
- NATS subjects: `agent.driver.extract_memories.completed` /
  `agent.driver.extract_memories.skipped`.
- Orchestrator wiring: `extract_memories` + `memory_dir` fields,
  builder setters, post-turn tick + gate check + spawn extraction,
  compact-turn path updated via `PostCompactCleanup`.
- 29 unit tests across `extract_memories.rs` + `extract_memories_prompt.rs`
  (manifest scan, memory-write detection, path resolution, response
  parsing, gate checks, circuit breaker, MEMORY.md index).
- Docs: Tier 4 added to `docs/src/ops/compact-tiers.md`.

Deferred / follow-up:
- LLM backend adapter (`ExtractMemoriesLlm` impl wrapping
  `nexo_llm::LlmClient`) — wired in the binary crate.
- Full message extraction (orchestrator currently passes `final_text`;
  the harness should surface recent conversation messages).
- `source: "extract:turn"` tag and git-backed memory audit (Phase
  10.9 follow-up).
- Integration test with synthetic 10-turn conversation.

#### 77.6 — memdir findRelevantMemories + memoryAge decay   ✅

Shipped:
- `MemoryType` enum (User/Feedback/Project/Reference) with
  `half_life_days()` — User/Reference = 10000d (∞), Feedback = 365d,
  Project = 90d. `parse()` for lenient deserialization from DB.
- `ScoredMemory { entry, score, freshness_warning }` struct in
  `crates/memory/src/relevance.rs` — separates storage from
  presentation.
- `score_memories(entries, similarity_scores, now, frequency_counts)`
  — composite scoring: similarity × recency(per-type half-life) ×
  log1p(frequency). Guards: NaN → 0.0, half-life=0 → recency=0.0,
  future mtime → age clamped to 0.
- `freshness_note(entry, now, threshold_days)` — `<system-reminder>`
  block when memory age > threshold. None when threshold is
  i32::MAX (disabled).
- `find_relevant(agent_id, query, limit, already_surfaced,
  freshness_threshold_days)` on `LongTermMemory` — wraps
  `recall_hybrid()` → `score_memories()` → filter surfaced →
  freshness_note → top-N.
- `memory_type TEXT` column in `memories` table — idempotent
  migration via `is_duplicate_column_error()`. `remember_typed()`
  stores it; all hydration paths (FTS + vector) read it.
- `aggregate_signals()` now accepts `half_life_days` parameter
  instead of hardcoded `7.0`. `recall_signals()` looks up
  per-memory type from DB.
- `already_surfaced: HashSet<Uuid>` in `Session` with
  `mark_surfaced()`, `is_surfaced()`, `surfaced_set()` helpers.
- 13 unit tests in `relevance.rs` covering NaN guard, zero half-life,
  future mtime, legacy None type, sorted ordering, user-type no-decay,
  freshness threshold boundary cases.
- 3 unit tests in `session/types.rs` for already-sfaced tracking.
- `aggregate_signals` recency test updated to use parameterized
  half-life.

Files: `crates/memory/src/relevance.rs` (new, ~200 lines),
`crates/memory/src/long_term.rs` (DB migration, `memory_type` field,
`remember_typed()`, `find_relevant()`, `frequency_counts_for()`,
per-type half-life in `aggregate_signals()`),
`crates/memory/src/lib.rs` (re-exports),
`crates/core/src/session/types.rs` (`already_surfaced` field).

#### 77.7 — memdir secretScanner + teamMemSecretGuard   ✅

Before any memory entry is committed (Phase 10.9 git-backed
write path or `crates/memory/src/long_term.rs::insert`),
scan the body for high-entropy strings, AWS / GCP / Stripe /
GitHub / Anthropic key shapes, and JWTs. Block on hit, emit
`memory.secret.blocked` event. Reference:
`services/teamMemorySync/secretScanner.ts` +
`teamMemSecretGuard.ts`.

Done when:
- Detection regex set ported + unit-tested with
  Anthropic / OpenAI / GitHub / AWS / Stripe / Google /
  generic-32-byte fixtures.
- Block path returns `MemoryError::SecretSuspected` with the
  matched detector name (no fragment of the secret in the
  error string).
- Docs note the limitation (regex only — not a sandboxed
  scanner).

#### 77.8 — bashSecurity destructive-command warning   ✅

16 compiled regex patterns (LazyLock) detect known destructive
git / rm / SQL / infra commands. `check_destructive_command()`
returns first-match warning. Integrated into
`PermissionMcpServer::call_tool` — when `tool_name == "Bash"`,
extracts `input["command"]`, runs check, attaches `warning` to
the permission JSON response. Feature-gated via
`PermissionMcpServer::bash_destructive_warning(bool)` (default
on). Purely informational — does not affect allow/deny.
Implemented in `crates/driver-permission/src/bash_destructive.rs`,
wired in `mcp.rs`.
Warn (Phase 67.D capability gate already blocks; this adds
a structured warning surfaced to the operator). Reference:
`tools/BashTool/destructiveCommandWarning.ts` +
`commandSemantics.ts`.

Done when:
- Classifier emits `bash.destructive_intent` with a tag set
  per detected pattern.
- Unit tests cover 20+ canonical destructive patterns +
  20+ false-positive look-alikes.

#### 77.9 — bashSecurity sed-in-place + path validation   ✅

Port `sedEditParser.ts` + `sedValidation.ts` +
`pathValidation.ts` (≈ 620 LOC Rust) — sed allowlist/denylist,
sed-in-place warning, and 29-command path extraction with POSIX
`--` handling.

Implemented:
- `sed_validator.rs` — `sed_command_is_allowed`, `extract_sed_expressions`,
  `has_sed_file_args`, `contains_dangerous_operations` (denylist)
- `path_extractor.rs` — `PathCommand` (29 variants), `extract_paths`,
  `filter_out_flags`, `classify_command`, `parse_command_args`
- `check_sed_in_place` in `bash_destructive.rs` — detects `sed -i` / `--in-place`
- Warning merged with 77.8 destructive warning in `PermissionMcpServer::call_tool`
- 55+ unit tests across sed validator, path extractor, and sed-in-place detection

Done when:
- [x] sed allowlist (line-printing + substitution patterns)
- [x] sed denylist (w/W write, e/E execute, backslash tricks, non-ASCII, etc.)
- [x] sed-in-place warning (`sed -i` / `--in-place` detection)
- [x] 29-command path extraction with POSIX `--` handling
- [x] All 105 driver-permission tests pass, workspace tests pass

#### 77.10 — bashSecurity shouldUseSandbox heuristic   ✅

Heuristic decides whether to wrap a Bash command in a sandbox
(bubblewrap / firejail on Linux when present). Pure decision
function — actual command wrapping is out of scope.
Reference: `tools/BashTool/shouldUseSandbox.ts`.

Implemented:
- `should_use_sandbox.rs` — `SandboxMode` (Auto/Always/Never),
  `SandboxBackend` (Bubblewrap/Firejail/None), `SandboxProbe`
  (one-shot PATH probe caches result), `should_use_sandbox()`
  decision function
- Excluded-commands support: prefix/exact match, env-var stripping
- 17 unit tests covering all modes, edge cases, env-var stripping

Done when:
- [x] `SandboxMode` enum with Auto/Always/Never variants
- [x] `SandboxProbe` probes for bwrap/firejail once, caches result
- [x] `should_use_sandbox()` pure decision function
- [x] Excluded commands matching (exact + prefix patterns)
- [x] 125 driver-permission tests pass, workspace tests pass

#### 77.11 — llmAiLimits + rateLimitMessages UX   ✅

Port the structured rate-limit / quota messaging from
`services/claudeAiLimits.ts` + `rateLimitMessages.ts` into
the shared LLM error layer (`crates/llm/src/retry.rs` +
provider adapters) so 429 + 529 + quota-exceeded responses
across providers render a humane diagnostic in `setup doctor`
(retry-after countdown, provider/plan cap context, plan
hint when known).

Done when:
- Shared error classification gains a provider-aware
  `LlmError::QuotaExceeded { provider, retry_after, plan_hint }`
  variant.
- Anthropic / OpenAI-compat / Gemini / MiniMax adapters map
  known quota payloads to that variant (unknown payloads keep
  existing generic fallback).
- `setup doctor` and the agent registry's `notify_origin`
  both surface the friendly message instead of generic
  "no quota".

#### 77.12 — Skill `loop` (auto-iteration)   ✅

Bundle a new skill at `skills/loop/` that takes
`{prompt, max_iters, until_predicate}` and runs the prompt
N times or until a predicate (regex / tool exit / LLM
judge) is satisfied. Reference:
`claude-code-leak/src/skills/bundled/loop.ts`.

Done when:
- Skill manifest + `phase()` impl + unit tests.
- Registered in Phase 13 skill index + admin-ui PHASES.md.

Shipped:
- Added `skills/loop/SKILL.md` with explicit input contract
  (`prompt`, `max_iters`, `until_predicate`) and bounded
  auto-iteration execution rules (parsing priority, guardrails,
  structured output).
- Added unit test
  `crates/core/src/agent/skills.rs::bundled_loop_skill_manifest_loads`.
- Added setup skill catalog registration
  (`crates/setup/src/services/skills.rs::id="loop"`) so the skill
  can be attached from `nexo setup` without manual YAML edits.
- Registered in `docs/src/skills/catalog.md` and
  `admin-ui/PHASES.md`.

#### 77.13 — Skill `stuck` (auto-debug)   ✅

Bundle a new skill at `skills/stuck/` that, given a recent
failure context (build error, test failure), runs a
diagnostic loop: re-run with verbose flags, grep error
strings, propose a fix candidate. Reference:
`claude-code-leak/src/skills/bundled/stuck.ts`.

Done when:
- Skill works against `cargo build` and `cargo test`
  failures end-to-end in the Phase 67 self-driving loop.

Shipped:
- Added `skills/stuck/SKILL.md` with explicit debug contract
  (`failing_command`, `max_rounds`, `focus_pattern`) and a bounded
  diagnosis workflow (`reproduce -> verbose -> isolate -> classify ->
  propose fix -> verify`).
- Added unit test
  `crates/core/src/agent/skills.rs::bundled_stuck_skill_manifest_loads`.
- Added setup skill catalog registration
  (`crates/setup/src/services/skills.rs::id="stuck"`) so the skill
  can be attached from `nexo setup` without manual YAML edits.
- Registered in `docs/src/skills/catalog.md` and
  `admin-ui/PHASES.md`.

Deferred:
- Full Phase 67 self-driving end-to-end run over real failing
  `cargo build` / `cargo test` traces (unit-level coverage is shipped).

#### 77.14 — Skill `simplify`   ✅

Bundle a code-simplification skill at `skills/simplify/`
that takes a file or hunk and proposes a smaller / clearer
version (renames, dead-code, redundant guards). Reference:
`claude-code-leak/src/skills/bundled/simplify.ts`.

Shipped:
- Added `skills/simplify/SKILL.md` with explicit simplification
  contract (`target`, `scope`, `max_passes`, `preserve_behavior`,
  `focus`) and bounded behavior-preserving cleanup workflow.
- Added unit test
  `crates/core/src/agent/skills.rs::bundled_simplify_skill_manifest_loads`.
- Added setup skill catalog registration
  (`crates/setup/src/services/skills.rs::id="simplify"`) so the skill
  can be attached from `nexo setup` without manual YAML edits.
- Registered in `docs/src/skills/catalog.md` and
  `admin-ui/PHASES.md`.

Deferred:
- Full Phase 67 self-driving end-to-end simplification replay over
  representative multi-file diffs (unit-level coverage is shipped).

#### 77.15 — Skill `verify`   ✅

Bundle a verification skill at `skills/verify/` that takes
an acceptance criterion in plain English and runs the
matching commands (test, lint, type-check) plus an LLM
judge over the output. Pairs with Phase 75 acceptance
autodetect. Reference:
`claude-code-leak/src/skills/bundled/{verify,verifyContent}.ts`.

Shipped:
- Added `skills/verify/SKILL.md` with explicit verification contract
  (`acceptance_criterion`, `candidate_commands`, `max_rounds`,
  `judge_mode`, `fail_fast`) and bounded evidence-first judge workflow.
- Added unit test
  `crates/core/src/agent/skills.rs::bundled_verify_skill_manifest_loads`.
- Added setup skill catalog registration
  (`crates/setup/src/services/skills.rs::id="verify"`) so the skill
  can be attached from `nexo setup` without manual YAML edits.
- Registered in `docs/src/skills/catalog.md` and
  `admin-ui/PHASES.md`.

Deferred:
- Full Phase 67 self-driving end-to-end acceptance replay over
  representative multi-step traces (unit-level coverage is shipped).

#### 77.16 — AskUserQuestion mid-turn elicitation tool   ✅

New tool in `crates/dispatch-tools/` that pauses the agent
loop, posts a question to the originating channel
(WhatsApp / Telegram / email / pairing companion-tui), and
resumes when the answer arrives. Hooks into Phase 14
TaskFlow wait/resume. Reference:
`claude-code-leak/src/tools/AskUserQuestionTool/`.

Done when:
- Tool survives daemon restart (state in
  `agent-registry`'s SQLite store).
- WA + TG adapters route the answer back to the right
  goal_id without manual correlation.
- Timeout knob (`ask.timeout_secs`, default 3600) escalates
  to `notify_origin` `[abandoned]` on expiry.

#### 77.17 — Versioned schema migrations system   ✅

`crates/config/` grows a `migrations/` module modelled on
`claude-code-leak/src/migrations/` (11 idempotent migration
fns). Boot reads the YAML's `schema_version`, applies any
pending migrations to a working copy, and writes back if the
operator opted in (`config.migrations.auto_apply: true`)
or prints the diff for manual review otherwise.

Done when:
- Migration fns are pure (`fn(YamlValue) -> YamlValue`),
  unit-tested with before/after fixtures.
- `nexo setup migrate` CLI subcommand applies them with
  `--dry-run` and `--apply` flavors.
- Phase 18 hot-reload re-validates the post-migration
  snapshot before swapping.

#### 77.18 — coordinator / worker mode pattern   ✅

In `crates/core/agent/` (or driver-loop), add a binding-level
role switch: `role: coordinator | worker`. Coordinators get
the full tool surface; workers see a curated subset
(no `team_create`, no `send_message` outside their parent,
no `sleep`). Mode mismatch on session resume flips back
gracefully. Reference:
`claude-code-leak/src/coordinator/coordinatorMode.ts`.

Done when:
- `role` declared in YAML, validated at boot (`coordinator |
  worker | proactive`; invalid values fail startup validation).
- Worker tool subset enforced by effective policy:
  default allowlist is `[bash, file_read, file_edit,
  agent_turns_tail]` when a worker binding omits
  `allowed_tools`; operator can still override via
  `inbound_bindings[].allowed_tools`.
- Worker disallow guard strips dangerous worker-incompatible tools
  from overrides (`Sleep`, `TeamCreate`, `TeamSendMessage`,
  `send_message`; strict model forbids worker-side direct send).
- Integration coverage verifies worker role receives curated tool
  surface at runtime (`worker_role_uses_curated_default_tools_in_runtime`);
  unit coverage verifies default subset + disallowed stripping.

#### 77.19 — docs + admin-ui sync   ✅

- `docs/src/` sync landed:
  - proactive mode page (`docs/src/agents/proactive-mode.md`)
  - compact tiers (`docs/src/ops/compact-tiers.md`)
  - memdir scanner status page (`docs/src/ops/memdir-scanner.md`)
  - bash safety knobs (`docs/src/ops/bash-safety.md`)
  - migrations CLI status page (`docs/src/cli/migrations.md`)
  - four new-surface tool docs already tracked under architecture:
    ToolSearch, TodoWrite, SyntheticOutput, NotebookEdit
    (+ RemoteTrigger companion page)
- `admin-ui/PHASES.md` now includes explicit runtime knobs for:
  - `llm.context_optimization`
  - proactive mode
  - binding role switch (`coordinator|worker|proactive`)
- Setup capability policy sync:
  - dangerous self-enable paths are blocked in
    `crates/setup/src/capabilities.rs` denylist
    (`proactive.enabled`, `binding.*.proactive.enabled`)
  - env-toggle inventory remains focused on env-gated extension
    capabilities.

#### 77.20 — proactive mode + adaptive Sleep tool   ✅

Port Claude Code's `--proactive` / KAIROS feature into nexo-rs
so a binding can run autonomously: the agent receives periodic
`<tick>` injections from the driver-loop, decides whether to
do useful work or call a new `Sleep { duration_ms, reason }`
tool that pauses the goal and schedules a wake-up. Unlike
Phase 7 Heartbeat (cron-style Rust callback) and Phase 20
`agent_turn` poller (cron-style scheduled LLM turn → channel),
proactive mode is **agent-driven self-pacing**: the model owns
its own cadence and explicitly reasons about cache cost vs.
work backlog. References:
- `claude-code-leak/src/main.tsx:2197-2204` (system-prompt
  injection) and `:3832-3833`, `:4612-4618` (CLI flag wiring).
- `claude-code-leak/src/tools/SleepTool/prompt.ts` (the Sleep
  tool description, including the 5-minute prompt-cache TTL
  trade-off the model must weigh).
- The leaked `src/proactive/` module body itself was
  dead-code-stripped from the npm artifact via
  `feature('PROACTIVE')` — only its public surface
  (`isProactiveActive`, `activateProactive('command')`) is
  referenced from `main.tsx`.

Built on top of: Phase 7 Heartbeat (interval primitive),
Phase 20 `agent_turn` poller (scheduled LLM turn machinery),
Phase 67 driver-loop (turn replay + acceptance hooks).
Mutually exclusive with the 77.18 coordinator role on the
same binding (a coordinator already owns its own scheduling).

Done when:

- New per-binding YAML block (validated through Phase 16
  `EffectiveBindingPolicy`):
  ```yaml
  proactive:
    enabled: true
    tick_interval_secs: 600        # base period between ticks
    jitter_pct: 25                 # ±25 % uniform jitter
    max_idle_secs: 86400           # hard cap before forced wake
    initial_greeting: true         # mirror Claude Code's "briefly greet the user"
    cache_aware_schedule: true     # bias durations toward cache window
  ```
- Driver-loop emits a `<tick>` user-role message into the
  goal's session at every interval (jittered). System prompt
  prepends the canonical proactive snippet
  (`"You are in proactive mode. Take initiative — explore,
  act, and make progress without waiting for instructions.
  You will receive periodic <tick> prompts…"`) when
  `proactive.enabled: true`. Snippet is gated identically to
  Phase 15.9's Claude-Code spoof — only injected when the
  binding actually opts in.
- New tool registered in `crates/dispatch-tools/`:
  ```rust
  Sleep { duration_ms: u64, reason: String }
  ```
  - Returns immediately with `tool_result` so the loop closes
    the turn cleanly; the goal is then parked in a new
    `GoalState::Sleeping { wake_at, reason }` on the agent
    registry (Phase 71 persistence keeps it across
    daemon restart).
  - On wake, driver-loop resumes the goal with a `<tick>`
    injection carrying the elapsed-since-sleep delta and the
    stored `reason` so the model has continuity context.
  - Bounds: `[60_000, 86_400_000]` ms (1 min – 24 h);
    requests outside the range are clamped with a warn.
- Cache-aware scheduler (when `cache_aware_schedule: true`):
  - For `duration_ms ∈ [60_000, 270_000]` → keep as-is
    (within the Anthropic 5-minute cache TTL).
  - For `duration_ms ∈ (270_000, 1_200_000)` → snap to one
    of `{270_000, 1_200_000}` (whichever is closer), with
    a `tracing::info!(target = "proactive", "cache-aware
    snap from … to …", reason)` log so the operator can
    audit decisions.
  - For `duration_ms > 1_200_000` → keep as-is (already
    amortising the cache miss).
  - The reasoning policy is purely advisory at the runtime
    layer — the model's prompt also documents the trade-off
    so reasoning happens at decision time too.
- Interrupt path: an inbound user message on the goal's
  origin channel (or any direct dispatch) cancels the pending
  wake-up timer, marks the sleep `interrupted`, and resumes
  the loop with a real user message instead of `<tick>`.
- Mode-mismatch guard on session resume: if the persisted
  goal was created with `proactive: true` and the current
  binding has `proactive.enabled: false`, the loop logs
  `proactive.deactivated_on_resume` and finishes the goal
  cleanly (no tick injection).
- Telemetry: counters `proactive.tick.fired`,
  `proactive.sleep.entered`, `proactive.sleep.interrupted`,
  `proactive.cache_aware.snapped` exposed via Phase 9.2
  metrics.
- Tests:
  - Unit: cache-aware snap covers the four windows
    (under-cache, mid-zone snap-down, mid-zone snap-up,
    over-cache).
  - Unit: `Sleep` clamps out-of-range durations and the
    warning is emitted exactly once.
  - Integration (`crates/driver-loop/tests/proactive_tick_test.rs`):
    a synthetic binding with `tick_interval_secs: 1` runs
    for 5 ticks; the model's mock alternates "do nothing"
    (calls `Sleep`) and "do work" (writes to the
    `project-tracker`); asserts `goal_state` flips
    `Sleeping` ↔ `Running` correctly.
  - E2E: a coordinator goal that flips to proactive on
    resume returns `BindingPolicyError::ConflictingRoles`
    instead of double-scheduling.
- Capability inventory: register
  `proactive.enabled` as an operator-visible toggle in
  `crates/setup/src/capabilities.rs::INVENTORY` (it isn't
  destructive but it does change cost characteristics —
  every wake-up is a billed turn).
- Docs: `docs/src/agents/proactive-mode.md` (new page,
  registered in `SUMMARY.md`) covers the YAML schema, the
  Sleep tool's description, the cache-aware schedule, and
  the interaction with Phase 20's `agent_turn` poller (the
  two are complementary — `agent_turn` for cron-driven
  external triggers, `proactive` for self-paced autonomy).
- Admin-ui: `admin-ui/PHASES.md` gains a "Proactive mode"
  bullet under the runtime knobs section.

Implementation slices:

- 77.20.1 ✅ — Config + prompt + Sleep base. `ProactiveConfig`
  now carries the full YAML surface (`enabled`,
  `tick_interval_secs`, `jitter_pct`, `max_idle_secs`,
  `initial_greeting`, `cache_aware_schedule`,
  `allow_short_intervals`, `daily_turn_budget`) with the
  Phase 77.20 defaults. `EffectiveBindingPolicy` resolves the
  per-binding override and `AgentContext` exposes
  `proactive_enabled` + `binding_role` for prompt injection.
  `SleepTool` mirrors the leak's prompt contract
  (`claude-code-leak/src/tools/SleepTool/prompt.ts`): use
  Sleep instead of `Bash(sleep ...)`, user can interrupt, ticks
  are check-ins. Bounds are now `[60_000, 86_400_000]` and the
  cache-aware snap covers the four required windows. `Sleep` is
  registered when the agent or any binding has proactive enabled.
- 77.20.2 ✅ — Sentinel interception + runtime tick loop. The
  driver loop now translates the Sleep sentinel into
  `AttemptOutcome::Sleep { duration_ms, reason }`, parks the goal
  in-process via `crates/driver-loop/src/proactive.rs`, wakes with a
  cancellation-aware timer, and prepends a synthetic `<tick>` block to
  the next Claude turn instead of feeding the sentinel JSON back as
  normal context. The in-process agent runtime now has the same
  primitive: `LlmAgentBehavior` maps the structured `Sleep` tool
  result into `AgentTurnControl::Sleep` before stringification, stops
  the LLM loop cleanly, and the per-session debounce task schedules an
  interruptible wake that injects a `RunTrigger::Tick` message from the
  `proactive` source. Human/adapter messages cancel pending sleep, and
  ticks flush immediately so they do not merge with user prompts.
- 77.20.3 ✅ — Persistent sleeping state. `AgentRunStatus` now has
  `sleeping`, and `AgentSnapshot.sleep` carries
  `{ wake_at, duration_ms, reason }`. The SQLite registry migrates
  additive indexed columns (`sleep_wake_at`, `sleep_duration_ms`,
  `sleep_reason`) while keeping the JSON snapshot back-compatible.
  `EventForwarder` marks a goal sleeping when it sees
  `AttemptOutcome::Sleep`, clears the state on the next
  `AttemptStarted`, and `reattach()` restores sleeping rows after a
  daemon restart as `ReattachOutcome::Sleeping`. `list_agents` /
  `agent_status` understand the new status, and shutdown drain treats
  sleeping goals as live work so they are not silently orphaned.
- 77.20.4 ✅ — Interrupt + budget + telemetry. User messages now cancel
  pending sleep (`sleep.interrupted`), proactive ticks obey
  `daily_turn_budget` (extra wakes are suppressed + re-armed), and
  telemetry exposes `nexo_proactive_events_total{agent,event}` for
  `tick.fired`, `sleep.entered`, `sleep.interrupted`,
  `cache_aware.snapped`. Coverage: `proactive_event_metrics_render`
  (telemetry) + `proactive_daily_turn_budget_suppresses_extra_ticks`
  (runtime integration).
- 77.20.5 ✅ — Integration/E2E + docs/admin-ui. Synthetic tick-loop
  integration coverage now includes `orchestrator_sleep_tick_test`
  (driver-loop Sleep→wake path) and
  `proactive_daily_turn_budget_suppresses_extra_ticks` (runtime budget
  guard). Docs page added at `docs/src/agents/proactive-mode.md`
  (`SUMMARY.md` wired). Capability policy now blocks self-enabling
  proactive via ConfigTool denylist
  (`binding.*.proactive.enabled`, `proactive.enabled`), and
  `admin-ui/PHASES.md` now includes the "Proactive mode" runtime-knob
  bullet.

Out of scope for 77.20:
- "Always-on background swarm" (multiple proactive goals
  cooperating) — that belongs to a future Phase 79 if we
  need it; 77.20 lands the single-goal primitive only.
- Voice / TTS announcements on tick — out of scope per
  Phase 77 charter (no Voice/STT).

##### 77.20 — Why this matters (charter)

The gap proactive mode closes today: Phase 7 Heartbeat is a
blind Rust callback (no reasoning, no tool use), Phase 20
`agent_turn` is a rigid cron with the same prompt every
firing, and Phase 67 self-driving only starts when a human
issues an explicit goal. None of them give the agent
*self-paced autonomy* — the ability to live between user
messages and decide for itself when to act and when to
rest. 77.20 is the missing primitive: an agent that owns
its own cadence, reasons about cost vs. work backlog, and
sleeps consciously instead of being polled.

##### 77.20 — Concrete use cases (do not lose during planning)

These are the workloads 77.20 unlocks across the existing
nexo-rs plugin surface. Each one explains *why* a cron /
heartbeat / poller is insufficient and what proactive
mode adds.

1. **Always-on personal assistant on WhatsApp / Telegram.**
   Tick every 30 min: scan inbox + calendar + reminder
   store. If nothing urgent → `Sleep(3600s,
   "no urgent items")`. If something surfaces → push to
   the origin channel without waiting to be asked.
   Bridges the Phase 7 reminder primitive and the reactive
   message path — neither alone covers "noticed something
   on its own".

2. **Email triage autonomous loop (Phase 48 plugin).**
   Wake every 15 min, read inbox, classify (auto-reply
   draft via pairing, archive bounce, mark spam, defer).
   Adapts cadence: many incoming → short waits;
   user on vacation → long waits. Replaces a fixed poller
   with one whose period is decided by observed volume.

3. **Self-driving dev agent between commands (Phase 67).**
   While the operator is idle: check CI status, read new
   PR comments, run `cargo audit`, surface regressions.
   Phase 67 today only runs goal-driven; 77.20 lets it
   *also* fill the gaps between explicit `nexo run`
   invocations.

4. **Infra operator (Phase 13.22 docker, 13.23 proxmox).**
   Tick every 5 min, pull metrics, alert when CPU > 90 %
   sustained, Sleep otherwise. Replaces dumb-threshold
   alertmanager for cases where the LLM should reason
   about *context* (is the spike a deploy? a regression?
   a known maintenance window?) before paging anyone.

5. **Smarter heartbeat for messaging plugins (Phases 6 + 7).**
   Today `on_heartbeat()` fires Rust code blindly. With
   77.20: "this contact has been silent for 3 days —
   should I send a soft follow-up?" becomes a per-tick
   LLM judgment instead of a hard rule.

6. **Continuous learning loop (Phase 10 Soul).**
   Tick → `dreaming` (10.6) without a fixed cron. Model
   decides: "47 new transcripts since last dream, run
   extraction now" vs. "only 2, Sleep longer". Cadence
   matches signal volume instead of clock time.

7. **Companion-tui background work (Phase 26).**
   Pairing companion in proactive mode: operator opens
   the TUI, leaves the session running, agent works in
   the background and surfaces tick-by-tick progress
   notes. The TUI becomes a live observation pane, not
   a request/response shell.

##### 77.20 — Why this beats fixed cron / polling

| Fixed cron / poller | Proactive mode |
|---------------------|----------------|
| Same cadence every firing | Cadence adapts to backlog |
| Every fire = billed LLM call | `Sleep` skips wake-ups when idle |
| Cache TTL ignored | Snap to ≤ 270 s or ≥ 1200 s windows |
| No "nothing to do" exit | Model decides + logs reason |
| Interrupts ignored | Inbound user msg cancels sleep, resumes with real context |

##### 77.20 — Cost guardrails (mandatory at boot)

Proactive mode's failure mode is runaway billing — a
mis-configured binding tickeing every 60 s costs real
money. 77.20 must ship with the following rails enabled
by default:

- `tick_interval_secs` minimum 60, default 600. Lower
  values require an explicit operator opt-in via
  `proactive.allow_short_intervals: true` registered in
  `crates/setup/src/capabilities.rs::INVENTORY` so
  `nexo setup doctor capabilities` flags it.
- `max_idle_secs` hard cap (default 86400) — prevents a
  runaway loop where the model keeps choosing 24 h sleeps
  forever and the goal effectively dies silent.
- Per-binding daily turn budget: a new
  `proactive.daily_turn_budget` (default 200) is checked
  by the rate limiter before injecting a `<tick>`. Budget
  exhausted → tick suppressed + `proactive.budget.exhausted`
  event + `notify_origin` `[budget paused]`.
- Phase 9.2 metrics counter `proactive.tick.fired` plus
  a Phase 72 turn-log column `tick: true` so operators
  can audit cost contribution post-hoc.
- Phase 18 hot-reload re-evaluates the proactive block:
  flipping `enabled: false` cancels the next pending
  wake-up cleanly; flipping `tick_interval_secs` reschedules
  the next firing only (no retroactive churn).

##### 77.20 — Sequencing within Phase 77

77.20 should land *after* 77.4 (`promptCacheBreakDetection`)
because the cache-aware scheduler depends on knowing when
a provider/model prompt cache has actually broken, otherwise
the snap-to-270 s heuristic is operating blind. Suggested
order inside Phase 77 once 77.1–77.4 are done: 77.20 next,
then 77.5–77.7 (memory/extract/secret-scanner), then the
bash-safety trio (77.8–77.10), then skills + UX
(77.11–77.16), then schema migrations + coordinator
(77.17–77.18), then docs sync (77.19).

##### 77.20 — Effort estimate

~2-3 engineer days. Most machinery already exists:
Phase 20 owns scheduled LLM turns, Phase 71 owns goal
persistence across restarts, Phase 67 owns the driver-loop
turn replay. The new bits are: the `Sleep` tool itself,
the `GoalState::Sleeping` variant + registry migration,
the cache-aware scheduler with its four-window logic,
the conditional system-prompt injection (gated identically
to Phase 15.9's Claude-Code spoof), and the cost-guardrail
plumbing (budget + capability inventory entry).

### Phase 79 — Tool surface parity sweep (claude-code-leak tools)   ⬜

After cataloguing the 40 tools in
`/home/familia/claude-code-leak/src/tools/`, 13 of them have
no equivalent in nexo-rs and would materially expand what the
agent can do without leaving its turn. Phase 79 ports the
13 missing tools (plus the docs sync). Phase 77 covers the
*infrastructure* parity sweep (compact, memdir, bash safety,
proactive mode, etc.); Phase 79 is the *tool surface* sweep —
they are siblings, not nested.

The 27 tools we already have (`Agent`, `Bash`, `FileRead/Write/Edit`,
`Glob`, `Grep`, `MCP`, `Skill`, `SendMessage` ≈ delegate, `WebSearch`,
`Enter/ExitWorktree`, `TaskCreate/Update/Get/List/Stop/Output`,
plus the in-flight 77.16 `AskUserQuestion` and 77.20 `Sleep`) stay as
they are. Phase 79 only adds what's missing.

PowerShellTool (Windows-only) and WebFetchTool (Phase 21
covers user-shared URLs) are explicitly out of scope.

References: `claude-code-leak/src/tools/{EnterPlanMode,ToolSearch,SyntheticOutput,TodoWrite,LSP,TeamCreate,TeamDelete,ScheduleCron,RemoteTrigger,Brief,Config,McpAuth,ListMcpResources,ReadMcpResource,REPL,NotebookEdit}Tool/`.

#### 79.1 — EnterPlanMode + ExitPlanMode tools   ✅

Two paired tools that flip the agent into a read-only "plan"
mode mid-turn. While in plan mode, every mutating tool
returns a structured `PlanModeRefusal` so the model is
forced to articulate a plan before execution. `ExitPlanMode`
takes a `final_plan: String` argument — the plan is logged
to the Phase 72 turn log and surfaced to the operator
(or the pairing channel) for confirmation before mutating
tools re-arm.

**Reference (PRIMARY)**: `claude-code-leak/src/tools/EnterPlanModeTool/EnterPlanModeTool.ts`
+ `claude-code-leak/src/tools/ExitPlanModeTool/ExitPlanModeV2Tool.ts`
+ `claude-code-leak/src/utils/permissions/permissionSetup.ts:1458-1489`
(`prepareContextForPlanMode`)
+ `claude-code-leak/src/bootstrap/state.ts:157,1333-1338,1354-1359`
(plan-mode state primitives + exit attachment trigger).
**Reference (secondary)**: OpenClaw `research/` — no plan-mode
equivalent (grep confirmed, only `delivery-plan.ts` cron files
matched); design lifts entirely from the leak.

**PlanModeRefusal** (structured shape — NOT a string per
brainstorm decision **d**):
```rust
struct PlanModeRefusal {
    tool_name: String,
    tool_kind: ToolKind,             // Bash | FileEdit | Outbound | Delegate | Schedule | Config
    hint: &'static str,               // "Call ExitPlanMode { final_plan } when the plan is ready."
    entered_at: i64,                  // unix seconds
    entered_reason: PlanModeReason,
}

enum PlanModeReason {
    ModelRequested,
    OperatorRequested,
    AutoDestructive { tripped_check: String },
}
```
Refusal lands as a `tool_result` with `is_error: true` so the
provider's classification (Anthropic, MiniMax, OpenAI-compat,
Gemini) stays consistent.

**Mutating tools blocked while in plan mode** (canonical list
in `crates/core/src/plan_mode.rs::MUTATING_TOOLS`; boot-time
assert verifies every registered tool is classified —
addition without classification fails compile/boot per
brainstorm decision **b**):
- `Bash` when `commandSemantics.is_mutating == true`
  (Phase 77.8/77.9 already classifies this; default to
  blocking if the classifier returns `Unknown`).
- `FileWrite`, `FileEdit`, `NotebookEdit` (79.13).
- `program_phase`, `delegate_to`, `dispatch_followup`.
- 79.7 `ScheduleCron`, 79.8 `RemoteTrigger`, 79.10
  `Config { op: apply }`.
- Every plugin outbound (`whatsapp.send`, `telegram.send`,
  `email.send`, `browser.click/type/navigate`).

**Read-only tools allowed**: `FileRead`, `Glob`, `Grep`,
`Bash` reads (`commandSemantics.is_mutating == false`),
`WebSearch`, `Lsp` (79.5), `ListMcpResources` (79.11),
`ReadMcpResource` (79.11), `ToolSearch` (79.2),
`AskUserQuestion` (77.16), `Sleep` (77.20).

**Sub-agent semantics** (lift from leak —
`EnterPlanModeTool.ts:78-80` + `SendMessageTool.ts:449` +
`AgentTool/agentToolUtils.ts:90`; refined with OpenClaw
`research/src/acp/session-interaction-mode.ts:4-15`,
brainstorm decisions **g**+**h**):
- `EnterPlanMode` rejects with `PermissionDenied` unless
  `AgentContext.is_interactive() && parent_goal_id.is_none()`.
  `is_interactive()` returns `false` for: sub-agent goals,
  cron-spawned goals (Phase 7 heartbeat / 79.7
  ScheduleCron), poller-spawned goals (Phase 19/20). Only
  pairing/chat-rooted goals qualify — they have a live
  channel that can deliver approval.
- `delegate_to`, `TeamCreate` (79.6) spawn child goals with
  `plan_mode = Off` regardless of parent state. Child does
  read-only research for the parent's plan.

**Plan-mode hint per turn** (lift from leak's plan_mode
attachment, `bootstrap/state.ts:1354-1359`, brainstorm
decision **e**):
- Frozen suffix appended to the system prompt while
  `plan_mode == On`, cache-friendly:
  ```
  [plan-mode] Active. Read-only exploration. Mutating tools refuse with PlanModeRefusal. Call ExitPlanMode { final_plan } when ready.
  ```
- Same injection channel as 77.20 proactive prompt + 79.9
  Brief; flips on/off in `crates/llm/src/prompt_assembly.rs`
  based on `AgentContext.plan_mode`.

**Notify_origin format** (frozen — brainstorm decision **k**):
```
[plan-mode] entered at <RFC3339> — reason: <model|operator|auto-destructive:<check>>
[plan-mode] exited — plan: <first 200 chars>… (full plan in turn log #<turn_idx>)
[plan-mode] acceptance: pass|fail (<test summary>)
[plan-mode] refused tool=<name> kind=<kind>
```

**Tool shapes**:
```rust
EnterPlanMode { reason: Option<String> }
  → { entered_at: i64, mode_was: PriorMode }

ExitPlanMode { final_plan: String }   // ≤ 8 KiB; oversize → PlanTooLarge error
  → { unlocked_at: i64, plan_chars: usize, awaiting_acceptance: bool }
```

**Code touchpoints**:
- New module `crates/core/src/plan_mode.rs`: `PlanModeState`
  enum (`Off | On { entered_at, reason, prior_mode }`),
  `MUTATING_TOOLS` const list, `gate_tool_call(state,
  tool_name) -> Option<PlanModeRefusal>` helper, boot-time
  registry-vs-list assert.
- `crates/dispatch-tools/src/dispatcher.rs`: pre-tool hook
  consults `PlanModeState` and short-circuits with the
  canonical refusal (centralised gate per brainstorm
  decision **b**).
- `crates/dispatch-tools/src/builtins/plan_mode.rs` (new):
  the `EnterPlanMode` + `ExitPlanMode` tool entry points.
- `nexo-agent-registry`: new column `plan_mode TEXT` (JSON
  `PlanModeState`) on `goals` table.
- `crates/config/src/types/binding.rs`: `plan_mode:
  PlanModePolicy { enabled, auto_enter_on_destructive,
  default_for_role }`.
- `crates/llm/src/prompt_assembly.rs`: append the per-turn
  hint when `plan_mode.is_on()`.
- `crates/core/src/agent/context.rs`: `AgentContext.plan_mode:
  Arc<RwLock<PlanModeState>>` (mirror cache; SQLite is
  canonical).
- Wait/resume on operator approval reuses the **TaskFlow
  wait/resume primitive (Phase 14.4)** until 79.10's
  `approval_correlator.rs` ships (brainstorm decision **c**).

**Defaults per role** (brainstorm decision **f**, replaces
the original blanket `enabled: true`):
```yaml
# Per-binding plan_mode block
plan_mode:
  enabled: true                       # tool registered + reachable
  auto_enter_on_destructive: false    # opt-in; pairs with 77.8
  # The "actively gating" default — if unset, computed:
  #   role: coordinator  → default: on
  #   role: worker        → default: off (workers never enter their own plan)
  #   role: proactive     → default: off (Sleep + tick disrupted by gating)
  #   role: <unset>       → default: off
  default_active: ~
```

**Auto-enter-on-destructive UX** (brainstorm decision **j**):
- When `auto_enter_on_destructive: true` and the next call
  is classified destructive by 77.8, the dispatcher
  PRE-empts the call: returns a single `tool_result` whose
  body is the canonical refusal **with the auto-enter
  reason embedded** (`entered_reason:
  AutoDestructive { tripped_check }`). Plan-mode flips to
  `On` in the same step. Model gets one explanatory
  refusal, not a surprise on the second attempt.
- Soft dep on 77.8: when 77.8 not shipped, this knob
  silently no-ops and emits a `tracing::warn!` once at
  boot.

**Acceptance hook — fire-and-forget** (brainstorm decision
**i**, corrects original "synchronous gate"):
- `ExitPlanMode` unlocks plan-mode immediately on operator
  approval (operator already validated intent).
- Phase 75 acceptance autodetect runs in **background**:
  - On pass → `notify_origin` posts `[plan-mode]
    acceptance: pass (<summary>)`.
  - On fail → `notify_origin` posts `[plan-mode]
    acceptance: fail (<summary>)` + emits an opt-in
    operator command hint:
    `[plan-mode] revert plan_id=<…>` re-enters plan mode.
- Acceptance is **signal**, not gate. Blocking the unlock
  on a slow test suite would degrade pairing UX.

**Pairing-friendly approval path** (brainstorm decision
**c** — diff vs leak which kills plan-mode under
`KAIROS_CHANNELS`):
- `ExitPlanMode` publishes to `notify_origin`:
  `[plan-mode] approve plan_id=<ulid>  | reject reason=…`
- Operator replies on the same channel with:
  - `[plan-mode] approve plan_id=<ulid>` — unlocks +
    schedules background acceptance.
  - `[plan-mode] reject plan_id=<ulid> reason=<…>` — keeps
    plan-mode on, plan stays in turn log, model receives
    the rejection reason as a `user` turn whose body is
    the canonical reject-followup prompt (lift from
    OpenClaw `research/src/agents/bash-tools.exec-approval-followup.ts:27-40`):
    ```
    Plan rejected by operator. Reason: <reason>.
    Do not call ExitPlanMode again with the same plan.
    Adjust the plan based on the rejection reason and present a revised plan.
    ```
- Goal awaits via TaskFlow wait/resume (Phase 14.4 ✅);
  default timeout 24 h (`plan_mode.approval_timeout_secs`
  knob); on timeout → `[plan-mode] approval expired` +
  goal moves to `LostOnApproval` registry state.
- Once 79.10 ships, swap the wait/resume call site to use
  the shared `approval_correlator.rs` primitive (same
  module reused by ConfigTool).

**Plan size limit** (brainstorm decision **a**):
- `final_plan` capped at 8 KiB (`PLAN_MODE_MAX_PLAN_BYTES =
  8192`). Oversize → `PlanTooLarge { actual, max }` tool
  error; model retries with a shorter plan.
- If the cap proves too tight in real use, follow-up phase
  introduces `ExitPlanMode { final_plan_path: PathBuf }`
  variant pointing at a file written via FileWrite during
  plan mode (matches the leak's disk-backed approach).
  Tracked as a 79.1 follow-up, not in this sub-phase.

Done when:
- Per-binding YAML knob `plan_mode: { enabled,
  auto_enter_on_destructive, default_active,
  approval_timeout_secs }` (defaults per role table above).
- Centralised gate in dispatcher with boot-time registry
  check — adding a tool without classifying it as
  mutating/read-only fails boot with a clear error.
- `EnterPlanMode { reason }` and `ExitPlanMode { final_plan
  }` tools registered + visible in deferred-schema mode
  (79.2-aware: not deferred, core surface).
- Sub-agent guard: `EnterPlanMode` from a sub-agent context
  returns `PermissionDenied`.
- Sub-agent propagation: child goals always
  `plan_mode = Off`.
- Per-turn system-prompt hint injected while active.
- Plan-mode state persisted in `agent-registry.goals.plan_mode`
  so daemon restart preserves it (Phase 71 reattach honours
  it).
- Pairing approval path: `[plan-mode] approve|reject`
  pattern works on at least one channel (test against the
  pairing companion-tui adapter); 24 h timeout enforced.
- Background acceptance integration: `ExitPlanMode` schedules
  Phase 75 acceptance fire-and-forget; pass/fail surfaces
  via `notify_origin`.
- Auto-enter PRE-empts destructive Bash with embedded reason
  in a single tool_result (no second-call surprise).
- All four `notify_origin` formats produce the canonical
  strings.
- Tests:
  - `tests/plan_mode_block_destructive.rs`: goal calls
    `Bash("rm -rf …")` while in plan mode → receives
    `PlanModeRefusal { tool_kind: Bash, … }`.
  - `tests/plan_mode_exit_unlocks.rs`: `ExitPlanMode` with
    `final_plan` + operator approval message → unlocks;
    next mutating call succeeds.
  - `tests/plan_mode_persists_across_restart.rs`: enter
    plan mode, simulate restart, verify mode survives + the
    per-turn hint resumes.
  - `tests/plan_mode_subagent_guard.rs`: spawning a
    sub-agent inside a plan-mode parent → child has
    `plan_mode = Off`; sub-agent calling `EnterPlanMode`
    → `PermissionDenied`.
  - `tests/plan_mode_auto_enter.rs`: with
    `auto_enter_on_destructive: true`, a destructive Bash
    triggers a single refusal carrying
    `AutoDestructive { tripped_check }`, plan-mode is on
    in the next state read.
  - `tests/plan_mode_acceptance_async.rs`: ExitPlanMode
    unlocks immediately; acceptance result arrives
    asynchronously and posts the canonical notify line
    (use a stub acceptance hook).
  - `tests/plan_mode_oversize_plan.rs`: 9 KiB `final_plan`
    → `PlanTooLarge` error.
  - `tests/plan_mode_approval_timeout.rs`: approval message
    delayed past `approval_timeout_secs` → goal resolves
    `LostOnApproval` + canonical timeout notify.
  - Phase 72 turn-log row asserts the plan body, the
    refusal events, and the unlocking event are all
    present, in order.

#### 79.2 — ToolSearchTool (deferred schemas)   ✅ (MVP — provider filtering deferred)

Today every registered tool ships its full JSONSchema in the
system prompt — when the surface is wide (40+ tools after
Phase 13 + 77 + 79), that prompt grows into kilobytes of
token cost on every turn. `ToolSearchTool` lets us advertise
each tool with just `{name, one_line_description}` in the
prompt; the model loads the full schema on-demand via
`ToolSearch(query: "select:Foo,Bar")` or by keyword
(`"notebook jupyter"`).

Reference: `claude-code-leak/src/tools/ToolSearchTool/` +
`src/services/api/buildSystemPrompt.ts` (deferred-schema
injection) + `src/QueryEngine.ts:1840-1920` (tool catalog
build + search).

**Code touchpoints**:
- `crates/core/src/tool_registry.rs`: `ToolRegistration`
  gains `deferred: bool`.
- `crates/llm/src/anthropic.rs`, `crates/llm/src/minimax.rs`,
  `crates/llm/src/gemini.rs`, `crates/llm/src/openai_compat.rs`:
  `build_tools_payload()` filters deferred tools to a stub
  shape (see below).
- `crates/dispatch-tools/src/builtins/tool_search.rs` (new):
  the `ToolSearch` tool itself + per-turn counter on
  `AgentContext`.
- `crates/extension-protocol/src/manifest.rs`: extensions
  may declare `deferred: true` per exported tool.

**Stub shape in system prompt** (per deferred tool):
```json
{
  "name": "FooTool",
  "description": "<one-line summary, ≤120 chars>",
  "deferred": true,
  "fetch_via": "ToolSearch(select:FooTool)"
}
```

**Query grammar** (mirror the leak's three forms):
- `select:Foo,Bar` — exact-match by name (returns full
  schemas).
- `notebook jupyter` — keyword search over `(name,
  description)` tokenised; ranked by simple BM25-ish
  score; returns top `max_results` (default 5).
- `+slack send` — `+token` is required, remaining tokens
  rank.

Done when:
- `ToolRegistration.deferred` flag plumbed through every
  LLM body builder.
- Deferred tools appear in the system prompt as the stub
  shape above; full JSONSchema body never leaves the
  registry until requested.
- `ToolSearch { query: String, max_results: Option<usize> }`
  returns a single `tool_result` whose body is a
  `<functions>` block matching exactly the leak's encoding
  so the very next assistant turn can call the tool with no
  extra ceremony.
- Per-turn rate limit `tool_search.max_calls_per_turn` =
  5 (configurable per binding). 6th call returns
  `rate_limited` error; counter resets at turn boundary.
- Token-budget integration test: a synthetic MCP server
  with 80 tools loaded as deferred → system prompt tools
  block < 4 KiB; calling `ToolSearch(select:my_tool)`
  returns the schema; next turn invokes successfully.
- Existing core tools default `deferred: false` (zero
  behaviour change for the small surface).
- MCP-imported tools default `deferred: true` (Phase 12
  registry sets the flag on import).
- Phase 76.7 (server-side notifications + streaming)
  consumes this design when emitting `tools/list_changed`
  — only stubs are pushed, schemas fetched on demand.

#### 79.3 — SyntheticOutputTool   ✅

A tool that takes a JSONSchema and forces the model to
return a typed object matching it — the model "calls" the
tool with the structured object as the only argument; the
runtime echoes that object back as the goal's terminal
output. Cleanly closes goals whose contract is "produce
this struct", with no parsing of free prose.

Reference: `claude-code-leak/src/tools/SyntheticOutputTool/`.

**Validation library**: `jsonschema = "0.18"` (already a
transitive dep via `nexo-mcp`; pin in `crates/dispatch-tools`).
Draft 2020-12 support, error path includes JSONPath of the
offending field.

**Code touchpoints**:
- `crates/dispatch-tools/src/builtins/synthetic_output.rs`
  (new).
- `crates/poller-runtime/src/goal_spec.rs`: `GoalSpec` gains
  optional `terminal_schema: Option<Value>`. When set, the
  poller injects a system suffix forcing termination via
  `SyntheticOutput` with that schema.
- `nexo-agent-registry`: `goals.terminal_value` BLOB column
  storing the validated structured output for read-back.

**Tool shape**:
```rust
SyntheticOutput {
    schema: Value,         // optional if poller-side schema
    value: Value,
}
```

When `terminal_schema` is poller-set the model omits
`schema` (runtime compares against the poller's). When
free-form the model supplies both.

Done when:
- `jsonschema` validates `value` against `schema` before
  terminating; failure returns a `tool_result` error with
  the offending JSONPath + expected type so the model
  retries deterministically.
- Goal terminates with `terminal_value` set; downstream
  consumers (Phase 19/20 pollers, Phase 51 eval harness,
  Phase 67 driver-loop hooks) read the structured value
  directly — no prose parsing.
- Phase 19/20 pollers can require their goals to terminate
  via `SyntheticOutput` with the poller's expected schema.
- Phase 51 eval harness scores acceptance against the
  schema (per-field ok/err counts).
- Tests:
  - valid object passes;
  - invalid scalar (`expected number, got string`) returns
    a clear path-qualified error;
  - nested arrays + enums + `oneOf` unions supported;
  - poller-driven `terminal_schema` rejects a value that
    does not match.

#### 79.4 — TodoWriteTool (intra-turn scratch list)   ✅

Distinct from Phase 14 TaskFlow: `TodoWrite` is an
in-memory, per-goal todo list that the model owns and edits
turn by turn. TaskFlow is persistent and cross-session;
TodoWrite is scratch. The leak shows the model uses it to
coordinate sub-steps inside a long driver-loop turn without
spawning sub-goals (which are heavier — separate registry
row, separate budget).

Reference: `claude-code-leak/src/tools/TodoWriteTool/`.

**Persistence model**: in-memory on the goal's
`AgentContext.todo_list: Arc<RwLock<Vec<TodoItem>>>`, NOT a
new SQLite table. Phase 72 turn log captures snapshots; on
restart the latest snapshot is rehydrated from the turn log
so the model resumes with its last list intact (no separate
recovery path).

**Diff vs Phase 14 TaskFlow**:

| Trait | TodoWrite (79.4) | TaskFlow (Phase 14) |
|-------|------------------|---------------------|
| Lifetime | Per goal, in-memory | Persistent, cross-session |
| Owner | Model | Operator + model + flows |
| Schema | flat list | DAG with deps + waits |
| Use | Sub-step coordination inside a long turn | Multi-day work programs |

**Code touchpoints**:
- `crates/core/src/agent/context.rs`: `todo_list` field.
- `crates/dispatch-tools/src/builtins/todo_write.rs` (new).
- `nexo-turn-log`: serialise `TodoItem[]` into existing
  `goal_turns.metadata_json`.
- `crates/plugins/pairing/src/tui_adapter.rs`: render
  `[~] in_progress` items with the existing spinner glyph.

Done when:
- `TodoWrite { items: Vec<{ id, content, status:
  pending|in_progress|completed }> }` replaces the goal's
  current todo list (full-replace semantics, idempotent).
- Phase 72 turn log writes the snapshot per turn (existing
  `metadata_json` column, key `todo_snapshot`).
- Driver-loop renders the latest todo list as a markdown
  status update on `notify_origin` when a `pairing`
  binding subscribes (debounced — only emit when the
  diff vs last snapshot is non-empty).
- Hard cap: max 50 items per goal. 51st item rejected
  with `tool_result` error suggesting consolidation.
- Hard cap: `content` ≤ 200 chars per item.
- Restart hydration: on goal resume, read the latest
  `todo_snapshot` from the turn log and prime
  `AgentContext.todo_list`.
- Tests:
  - ordering preserved across turns;
  - `in_progress` rendered with a working spinner glyph
    in the pairing-tui adapter (Phase 26);
  - 51st item rejected;
  - restart hydration round-trips.

#### 79.5 — LSPTool   ✅

Run a Language Server Protocol query in-process: `go_to_def`,
`hover`, `references`, `workspace_symbol`, `diagnostics`.
Massive multiplier for Phase 67 self-driving dev — instead
of grepping symbol names the model can ask the LSP for
the exact definition.

Reference: `claude-code-leak/src/tools/LSPTool/` +
`claude-code-leak/src/services/lsp/`.

**Crate choice**: `async-lsp = "0.2"` (actively maintained,
tokio-native, transport-agnostic). NOT `tower-lsp-client`
(that's the *server* side; the client equivalent under
`tower-lsp` was renamed and is not published as a stable
crate as of 2026-01). Confirm with a `cargo search` before
spec.

**Code touchpoints**:
- `crates/lsp/Cargo.toml` (new): `async-lsp`, `lsp-types`,
  `tokio-process`.
- `crates/lsp/src/launcher.rs`: per-language `which`-probe
  + spawn + handshake (`initialize` → wait for `initialized`
  notification).
- `crates/lsp/src/session.rs`: one session per `(workspace
  root, language)`; idle reaper task.
- `crates/dispatch-tools/src/builtins/lsp.rs`: tool front.
- `crates/config/src/types/lsp.rs`: `LspPolicy { enabled:
  bool, languages: Vec<LspLanguage>, idle_teardown_secs:
  u64 }`.

**Server matrix** (binary name, language, default install
hint surfaced when missing):

| Language | Binary | Hint on missing |
|----------|--------|-----------------|
| rust | `rust-analyzer` | `rustup component add rust-analyzer` |
| python | `pylsp` | `pip install python-lsp-server` |
| typescript / javascript | `typescript-language-server` | `npm i -g typescript-language-server` |
| go | `gopls` | `go install golang.org/x/tools/gopls@latest` |

Done when:
- `crates/lsp` (new crate) wraps `async-lsp` with the
  per-language launcher above. Auto-probes binaries at boot
  and disables servers whose binary is missing
  (`tracing::warn!` once per missing).
- One LSP session per `(workspace_root, language)` tuple,
  lazy start on first call, idle teardown after
  `idle_teardown_secs` (default 600).
- Tool variants:
  - `Lsp { kind: "go_to_def", file, line, character }`
  - `Lsp { kind: "hover", file, line, character }`
  - `Lsp { kind: "references", file, line, character }`
  - `Lsp { kind: "workspace_symbol", query }`
  - `Lsp { kind: "diagnostics", file }`
- Diagnostics output normalised so all servers emit the
  same `{severity, range, message, source}` shape.
- Per-call timeout 30 s — LSP hangs degrade to clear
  `tool_result` error rather than block the turn.
- Phase 67 driver-loop opt-in flag (`lsp.enabled` per
  binding); off by default (some workspaces don't have
  LSP servers).
- E2E test: a Phase 67 goal "rename `Foo` to `Bar` across
  the crate" uses `references` then `FileEdit` per match
  and produces a clean diff.
- Unit test: missing binary → tool returns clear error
  with the install hint above.

#### 79.6 — TeamCreateTool / TeamDeleteTool   ✅ (MVP — spawn-as-teammate via Phase 67 dispatch deferred to 79.6.b)

Spawn a *team* of N parallel agents, each with a different
role, sharing a scratchpad dir for cross-agent message
passing. Distinct from `AgentTool` / our existing `delegate_to`
which is 1-to-1: a team is N-in-parallel coordinated. Direct
input for research fan-out, large refactors, multi-source
verification.

Reference: `claude-code-leak/src/tools/TeamCreateTool/` +
`TeamDeleteTool/` + `coordinator/coordinatorMode.ts`
(Phase 77.18).

**Hard dependencies** (cannot start before):
- Phase 67 multi-agent registry (✅ shipped).
- Phase 77.18 coordinator/worker mode pattern (⬜ pending) —
  `role: coordinator` gate is the access control for
  `TeamCreate`.

If 77.18 not yet shipped, 79.6 stays paused.

**Code touchpoints**:
- `nexo-agent-registry`: `goals.spawn_strategy` JSONB gains
  `TeamFanout` variant; new `team_id UUID` column on
  child goals.
- `crates/dispatch-tools/src/builtins/team_create.rs` +
  `team_delete.rs` (new).
- `crates/core/src/team_scratchpad.rs` (new): inotify watch
  + capped append-only file per worker
  (`worker-<n>.md`, max 256 KiB each).
- `crates/dispatch-tools/src/cancel.rs::cancel_agent`:
  extend to accept `cancel_team(team_id)`.

**Tool shape**:
```rust
TeamCreate {
    team_id: Option<Uuid>, // server-assigned if None
    workers: Vec<{
        role: String,
        prompt: String,
        tool_subset: Option<Vec<String>>,
        budget_tokens: Option<u64>,
    }>,
    shared_goal: String,
}
TeamDelete { team_id: Uuid, reason: String }
```

Done when:
- Hooks into Phase 67 multi-agent registry — a team is a
  parent goal whose `spawn_strategy: TeamFanout { workers
  }`.
- Shared scratchpad dir (`.nexo/team-<team_id>/scratch/`)
  with inotify so workers see each other's notes
  near-realtime. Each worker has an exclusive append-only
  file; cross-worker reads through `FileRead` (no shared
  writes — avoids contention).
- Phase 77.18 coordinator role is the natural parent of
  a team — `TeamCreate` returns `permission_denied` unless
  caller's binding `role == coordinator`.
- Per-worker budget cap (`budget_tokens`); when exhausted,
  worker terminates and team-level summary is published.
- `TeamDelete` cancels every worker cleanly via the
  extended `cancel_team` path; emits one `notify_origin`
  per worker.
- Tests:
  - 3-worker team that researches the same question from
    different sources converges (mocked LLM);
  - team_id propagates through the registry;
  - SIGTERM drains all team members in parallel (Phase 71
    drain helper extended);
  - non-coordinator caller gets `permission_denied`.

#### 79.7 — ScheduleCronTool   ✅

Agent-time scheduling: from inside a turn, the model can
register a cron entry that fires a future goal. Complements
Phase 7 Heartbeat (config-time) and Phase 20 `agent_turn`
poller (config-time) by allowing *runtime-driven* schedule
mutations like "remind me to check the build in 6 h".

Reference: `claude-code-leak/src/tools/ScheduleCronTool/` +
`utils/cronScheduler.ts` + `utils/cronJitterConfig.ts`.

**Diff vs Phase 7 Heartbeat vs Phase 20 agent_turn**:

| Mechanism | Trigger source | Mutable at runtime | Persists | Use |
|-----------|----------------|--------------------|----------|-----|
| Phase 7 Heartbeat | YAML `heartbeat.interval_secs` | No (hot-reload only) | Config | Periodic background ticks per agent |
| Phase 20 `agent_turn` poller | YAML cron spec | No (hot-reload only) | Config | Scheduled LLM turn → channel publish |
| **79.7 ScheduleCron** | LLM tool call mid-turn | Yes (model-driven) | SQLite table | Self-scheduled reminders, follow-ups, "check X in 6 h" |

ScheduleCron is the *only* one where the model itself
mutates the schedule.

**Code touchpoints**:
- `crates/scheduler/src/cron_store.rs` (new) —
  `nexo_cron` SQLite table:
  ```sql
  CREATE TABLE nexo_cron (
    id TEXT PRIMARY KEY,
    binding_id TEXT NOT NULL,
    goal_id TEXT,
    spec TEXT NOT NULL,
    prompt TEXT NOT NULL,
    channel TEXT,
    next_fire_at INTEGER NOT NULL,
    last_fired_at INTEGER,
    paused INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
  );
  ```
- `crates/scheduler/src/runtime.rs`: tokio task that polls
  `next_fire_at` every 5 s, fires due entries through
  `agent_turn` poller machinery (Phase 20).
- `crates/dispatch-tools/src/builtins/schedule_cron.rs`
  (new).
- `src/main.rs`: `nexo cron {list,drop,pause,resume}`
  subcommand.

Done when:
- `ScheduleCron { spec: cron|every:<duration>|at:<rfc3339>,
  prompt: String, channel: Option<String> }`. Spec parsing
  reuses `cron = "0.12"` for cron expressions; `every:` and
  `at:` parsed manually.
- Persisted in `nexo_cron` table — survives daemon restart;
  next-fire time recomputed at boot.
- Bounds: max 50 active entries per binding
  (`cron.user_max_entries` raises this, hard ceiling 500);
  minimum interval 60 s; rejected with clear `tool_result`
  error if violated.
- Jitter: ±10 % of the interval applied to `next_fire_at`
  to avoid thundering-herd when many goals schedule at the
  same `every:1h`.
- Capability gate: `cron.enabled` per binding (default
  `true` only when binding role is `coordinator` or
  `proactive`; default `false` otherwise — explicit
  operator opt-in).
- `nexo cron list / drop / pause / resume` CLI subcommand
  for operator inspection (mirrors the LLM-side state).
- Tests: persistence across restart, jittered firing,
  cancel via `nexo cron drop <id>` removes the entry +
  fires `cron.cancelled` event; cap rejection at the
  51st entry; minimum-interval rejection at 30 s.

#### 79.8 — RemoteTriggerTool   ✅

LLM-time webhook / NATS publish: from inside a turn, the
model triggers a configured remote endpoint. Surface for
"agent → outside world" integrations that aren't covered
by an existing plugin (CRM webhooks, Zapier-style fan-out,
internal NATS subjects on `agent.outbound.*`).

Reference: `claude-code-leak/src/tools/RemoteTriggerTool/`.

**Code touchpoints**:
- `crates/config/src/types/remote_triggers.rs` (new):
  `RemoteTriggerEntry { name, kind: Webhook | Nats, … }`.
- `crates/dispatch-tools/src/builtins/remote_trigger.rs`
  (new) — uses `reqwest` for webhook, `nexo-broker` for NATS.
- `crates/dispatch-tools/src/circuit_breaker.rs`: per-trigger
  breaker keyed by `name`.

**HMAC header** (matches leak's pattern): outbound webhook
sends:
- `X-Nexo-Signature: sha256=<hex(hmac_sha256(secret, body))>`
- `X-Nexo-Timestamp: <unix>`
- `X-Nexo-Trigger-Name: <name>`

Done when:
- `RemoteTrigger { name: String, payload: Value }` where
  `name` resolves through a per-binding YAML allowlist:
  ```yaml
  remote_triggers:
    - name: "ops-pager"
      kind: "webhook"
      url: "https://hooks.example.com/…"
      secret_env: "OPS_PAGER_SECRET"
      timeout_ms: 5000
    - name: "internal-nats"
      kind: "nats"
      subject: "agent.outbound.ops"
  ```
- Names not in the allowlist refuse with a clear error
  (no model-controlled URLs).
- Webhook signing via HMAC-SHA256 of the body using the
  resolved secret env var — Phase 17 per-agent credentials
  resolves the secret.
- Reuses the Phase 2.5 circuit breaker per trigger name;
  Phase 9.2 metrics expose
  `remote_trigger.{name}.{ok,err,latency_ms}`.
- Per-trigger rate limit (default 10 calls / minute,
  configurable via `rate_limit_per_minute`).
- Body size cap 256 KiB; oversized payload → tool error
  with size in message.
- Tests:
  - missing allowlist entry returns clear tool error;
  - successful webhook signs body + asserts on the three
    headers above (mock server);
  - NATS path reaches the expected subject (broker test
    harness);
  - rate limit kicks in on the 11th call within 60 s.

#### 79.9 — BriefTool (terse-mode toggle)   ⬜

Mid-turn toggle for terse output: model commits to short
fragments only until untoggled or the session ends. Pairs
with the Phase 26 pairing companion-tui where small screens
benefit from compact responses.

Reference: `claude-code-leak/src/tools/BriefTool/`.

**Code touchpoints**:
- `crates/dispatch-tools/src/builtins/brief.rs` (new).
- `crates/core/src/agent/context.rs`: `brief_state:
  BriefState` enum (`Off | Turn | Session`).
- `crates/llm/src/prompt_assembly.rs`: append the canonical
  brief suffix when `brief_state != Off`.
- `nexo-agent-registry`: `goals.brief_state TEXT`.

**Canonical brief suffix** (frozen string — same across all
LLM providers so cache stays warm):
```
Reply terse. Fragments OK. Drop articles. Code blocks unchanged. Errors quoted exact.
```

Done when:
- `Brief { enabled: bool, scope: turn|session }`.
- Adds the canonical brief suffix to the system prompt
  while enabled — same channel as the proactive prompt
  injection in Phase 77.20.
- `scope: turn` resets to `Off` at the next turn boundary;
  `scope: session` persists until explicitly disabled or
  goal terminates.
- Persisted on `agent-registry.goals.brief_state` per goal
  so daemon restart preserves it.
- Tests:
  - enabling shrinks the average reply length on a fixture
    conversation by ≥ 30 % (mocked LLM with token-count
    assertions);
  - `enabled: false` restores normal output;
  - `scope: turn` auto-clears next turn;
  - restart hydration round-trips.

#### 79.10 — ConfigTool (gated self-config)   ✅

Agent reads / proposes edits to its own YAML config from
inside a turn. Powerful (the conversational `nexo setup`
flow opens up — operator says "use Claude Opus for this
binding" in chat and the agent adjusts). Dangerous (a
model that can rewrite its own config can also revoke its
own guardrails).

Reference: `claude-code-leak/src/tools/ConfigTool/` +
`src/services/config/configWriter.ts` +
`src/migrations/` (versioning pattern).

**Hardcoded denylist** (cannot be touched even when
`config.self_edit: true`):
- Every key matching glob `*_token`, `*_secret`,
  `*_password`, `*_key` (case-insensitive on the suffix).
- `pairing.*` (every sub-key).
- `dispatch_policy.*`.
- `capabilities.*`.
- `mcp.servers.*.auth.*` and any `mcp.servers.*.command`
  (running arbitrary binaries via config self-edit is
  game-over).
- `binding.*.role` (cannot self-promote to coordinator).
- `binding.*.plan_mode.*` (cannot disable own plan-mode
  guard rails).
- `remote_triggers[*].url`, `remote_triggers[*].secret_env`.
- `cron.user_max_entries` (operator-only).
- `agent_registry.store.*` (changing the store under a
  running goal is unsafe).

The denylist is one source of truth in
`crates/setup/src/capabilities.rs::CONFIG_SELF_EDIT_DENYLIST`
with a unit test asserting every glob compiles + matches
its intent.

**Code touchpoints**:
- `crates/setup/src/yaml_patch.rs`: extend with
  `apply_patch_with_denylist` that returns
  `Err(ForbiddenKey { matched_glob })` on hits.
- `crates/dispatch-tools/src/builtins/config_tool.rs` (new).
- `.nexo/config-proposals/<patch_id>.yaml` staging dir.
- `crates/core/src/agent/approval_correlator.rs` (new):
  matches operator approval messages to staged proposals
  via `patch_id` — same channel as the operator who
  triggered the goal (Phase 26 pairing).

**Approval message shape** (operator channel):
```
[config-approve patch_id=01J… ] Apply
[config-approve patch_id=01J… ] Reject reason=…
```

Done when:
- Three operations: `Config { op: read, key }`,
  `Config { op: propose, patch: YamlPatch, justification:
  String }`, `Config { op: apply, patch_id }`.
- `read` honours the same env-var resolution + secret
  redaction as `nexo setup show`.
- `propose` writes the candidate diff to a staging file
  (`.nexo/config-proposals/<patch_id>.yaml`) and notifies
  the operator on `notify_origin` for approval; **never**
  applies live; expires after 24 h.
- `apply` requires a fresh operator-channel approval
  message correlated to `patch_id` — the model alone
  cannot promote a proposal. Approval message must come
  from the same `(channel, account_id)` tuple that owns
  the binding (Phase 17).
- Capability gate: `config.self_edit: bool` per binding
  (default `false`). Denylist enforcement at both
  `propose` (early reject) and `apply` (defence in depth).
- Phase 18 hot-reload re-validates the post-apply snapshot.
- Rollback: if validation fails post-apply, the previous
  snapshot is restored automatically and the operator is
  notified with the validation error.
- Audit: every `propose`/`apply`/`reject` writes a row to
  Phase 72 turn log + a dedicated `config_changes` SQLite
  table (binding_id, patch_id, op, actor, timestamp).
- Tests:
  - forbidden-key proposal returns clear `ForbiddenKey`
    error citing the matched glob;
  - valid proposal needs operator approval;
  - reload picks up the change;
  - rollback path triggers when the new snapshot fails
    validation;
  - 24 h expiry trims stale proposals;
  - cross-binding approval forgery rejected (binding A
    cannot approve binding B's proposal).
- Security review gate: this sub-phase ships behind a
  `--feature config-self-edit` Cargo flag until reviewed.
  `crates/setup` exports zero entry points until the
  feature is on.

#### 79.11 — McpAuth + ListMcpResources + ReadMcpResource   ✅ (MVP — McpAuth deferred, trait lacks refresh hook)

Three tools that expose the MCP resource surface to the
LLM. Today (Phase 12.5) MCP resources are accessible via
`crates/mcp` programmatically but not as LLM-callable
tools — the model can't navigate them.

Reference: `claude-code-leak/src/tools/McpAuthTool/` +
`ListMcpResourcesTool/` + `ReadMcpResourceTool/` +
`src/services/mcp/oauthPort.ts` (auth refresh flow).

**Code touchpoints**:
- `crates/dispatch-tools/src/builtins/mcp_resources.rs`
  (new): three tool entry points sharing a helper.
- `crates/mcp/src/client/resources.rs` (new): `list` /
  `read` wire calls.
- `crates/mcp/src/client/oauth.rs`: extend with
  `refresh_now()` returning the refreshed token's
  `expires_at`.
- `crates/config/src/types/mcp.rs`:
  `mcp.resource_max_bytes` (default 262144),
  `mcp.list_max_resources` (default 200).

**Tool shapes**:
```rust
ListMcpResources { server: Option<String> }
  → { resources: Vec<{ server, uri, mime, size_hint? }>, truncated: bool }

ReadMcpResource { server: String, uri: String }
  → { mime: String, body: ReadBody }   // ReadBody = Text(String) | Binary(Base64String)

McpAuth { server: String, op: "refresh" | "status" }
  → { state: "authenticated" | "expired" | "unauthorised", expires_at?: Rfc3339 }
```

Done when:
- `ListMcpResources { server: Option<String> }` lists every
  resource on every connected server (or just one). Cap at
  `mcp.list_max_resources`; truncation flagged with
  `truncated: true`.
- `ReadMcpResource { server, uri }` returns the resource
  body capped at `mcp.resource_max_bytes` (default 256
  KiB). Binary resources base64-encoded; text returned raw.
- `McpAuth { server, op: refresh|status }` triggers an
  OAuth refresh or reports auth state — useful for MCP
  servers behind expiring tokens.
- All three honour Phase 76 multi-tenant isolation +
  Phase 76.3 pluggable auth (per-principal allowlist on
  which servers are visible).
- Tests:
  - server with 5 resources surfaces correctly;
  - too-large resource returns a clear "exceeded cap"
    error with a hint to use a more specific URI;
  - server filter excludes resources from other servers;
  - `McpAuth` refresh on an unconfigured server returns a
    clear error;
  - Phase 76 isolation: caller A cannot read caller B's
    server resources.

#### 79.12 — REPLTool (stateful sandbox)   ✅

Python / Node REPL whose interpreter survives across turns
inside the same goal. Variables, imports, definitions
persist. Strong sandbox required because the model can
import any module.

Reference: `claude-code-leak/src/tools/REPLTool/` +
`src/services/sandbox/` (sandbox detection helpers).

**Sandbox matrix per OS** (refuse-to-start behaviour
depends on operator opt-in):

| OS | Default sandbox | Fallback | If neither present |
|----|----------------|----------|--------------------|
| Linux | `bwrap` (bubblewrap) | `firejail` | refuse with hint |
| macOS | `sandbox-exec` (built-in) | — | refuse with hint |
| Termux / Android | none viable | — | refuse |

Operator may opt out per binding via
`repl.allow_unsandboxed: true` (default `false`, requires
`config.self_edit`-equivalent friction — capability gate
hardcoded ⇒ only via direct YAML edit + restart, not
self-config).

**Code touchpoints**:
- `crates/dispatch-tools/src/builtins/repl.rs` (new).
- `crates/sandbox/src/lib.rs` (new): probes + spawn
  helpers; reuses 77.10 `shouldUseSandbox` detection when
  77.10 has shipped, otherwise stand-alone probe.
- `crates/dispatch-tools/src/repl_session.rs`: REPL
  session manager keyed `(goal_id, language)`.

**REPL implementation**:
- `python3 -i -u` with line-buffered stdio, IPC framing
  via `\x1e` record separator + magic-string sentinels
  (no PTY — stdio is enough for stateful REPL).
- `node --interactive` similar.

**Tool shape**:
```rust
Repl {
    language: "python" | "node",
    code: String,
    timeout_ms: Option<u64>,  // default 10000, cap 60000
}
  → {
      stdout: String,
      stderr: String,
      value: Option<Value>,    // last expression value, JSON-encoded if scalar/dict
      duration_ms: u64,
      truncated: bool,         // true if stdout/stderr hit 64 KiB cap
    }
```

Done when:
- One sandboxed subprocess per `(goal_id, language)` tuple.
- Sandbox per OS matrix above. Strict default: no network,
  RW limited to `/tmp/repl-<goal_id>/` (or
  `${TMPDIR}/repl-<goal_id>/` on macOS), RO `/usr` +
  `/etc/ssl` (Linux), `/usr` + `/etc` + `/Library` (macOS).
- Languages: `python3` first (largest demand), `node`
  second. No shell language.
- `Repl { language, code, timeout_ms }` returns the shape
  above. Errors carry the canonical traceback truncated
  to 2 KiB.
- Idle teardown after 10 min. Hard cap: 1 REPL per goal,
  per language → max 2 subprocesses per goal.
- Output caps: stdout/stderr each 64 KiB per call;
  truncation flagged with `truncated: true`.
- Capability gate: `repl.enabled: bool` per binding
  (default `false`). On platforms with no sandbox the
  tool refuses with a hint to install `bwrap`/`firejail`
  or set `repl.allow_unsandboxed: true`.
- Tests:
  - variable carry-over across turns;
  - sandbox blocks network attempt with a clear error
    (Linux only — gated by `cfg(target_os = "linux")`);
  - teardown after idle;
  - timeout returns clear error + kills child;
  - 65 KiB stdout truncates and flags;
  - non-Linux platform without `sandbox-exec` refuses to
    start.

#### 79.13 — NotebookEditTool   ✅

Cell-level edits on Jupyter `.ipynb` files preserving
outputs and metadata. Niche but cheap — the read path is
already covered by `FileRead` (which the leak's
`FileReadTool` extends to handle notebooks).

Reference: `claude-code-leak/src/tools/NotebookEditTool/`.

**Implementation choice**: pure-Rust JSON parse via
`serde_json` against the public `nbformat 4.5` schema —
no `jupyter` binary required. The notebook is a
well-defined JSON document; round-trip via
`serde_json::Value` preserves unknown fields automatically
(forward-compat with newer nbformat).

**Code touchpoints**:
- `crates/dispatch-tools/src/builtins/notebook_edit.rs`
  (new).
- `crates/notebook/src/lib.rs` (new, small): `Notebook`,
  `Cell`, `CellId`, `apply_edit`. Pure data ops, no IO.

**Tool shape**:
```rust
NotebookEdit {
    file: PathBuf,
    cell_id: String,
    op: "replace" | "insert_after" | "delete",
    content: Option<String>,         // required for replace/insert_after
    cell_type: Option<"code" | "markdown" | "raw">,  // for insert_after
}
  → { file, cell_id_after_edit?, total_cells }
```

Done when:
- Pure-Rust round-trip: read `.ipynb` → mutate cells →
  write back with stable formatting (`serde_json` with
  pretty + 1-space indent matching Jupyter's default).
- Outputs of edited cells cleared (so the diff stays sane);
  outputs of untouched cells preserved.
- `cell_id` lookup falls back to positional index
  (`"0"`, `"1"`, …) when no UUID `cell_id` is present
  (older notebooks).
- Round-trip preserves unknown JSON fields (nbformat
  forward-compat).
- Tests:
  - edit a cell, output diff is bounded
    (only the targeted cell + its outputs change);
  - insert_after preserves cell ordering;
  - delete reduces `total_cells` by 1;
  - unknown top-level keys round-trip unchanged;
  - missing `cell_id` returns a clear error listing
    available IDs.

#### 79.M — MCP exposure parity sweep   ✅ (MVP — Lsp/Team*/Config wiring deferred to 79.M.b/c/d)

Closes the gap between the runtime tool registry (`nexo run`,
~31 tools) and the surface advertised to external MCP clients
via `nexo mcp-server` (previously 12 tools max via the legacy
`expose_tools` match arm). Source-of-truth is now the
`EXPOSABLE_TOOLS: &[ExposableToolEntry]` slice in
`crates/config/src/types/mcp_exposable.rs`. Boot dispatch
(`crates/core/src/agent/mcp_server_bridge/dispatch.rs`) walks
the slice once per server start, delegates to per-tool boot
helpers in the `Always` arm, and surfaces three categorical
skip reasons (`denied_by_policy`, `deferred`, `infra_missing`,
`feature_gate_off`, `unknown_name`) so the operator sees a
labelled warn line for every entry the server refused.

Reference (PRIMARIO):
- `claude-code-leak/src/Tool.ts:395-449` — gating signals
  (`isReadOnly`, `isMcp`, `isLsp`, `shouldDefer`) inspired
  the per-entry `SecurityTier` + `BootKind`.
- `claude-code-leak/src/services/mcp/channelAllowlist.ts:1-80` —
  hard-coded operator-non-editable allowlist; mirrors our
  `EXPOSABLE_TOOLS` slice.

Reference (SECUNDARIO):
- `research/docs/cli/mcp.md:30-120` — `openclaw mcp serve`
  curated catalog (`conversations_list`, `messages_read`,
  `events_poll`, `events_wait`, `messages_send`); informs
  the "catalog ≠ runtime registry" choice.

Three-bucket policy (hard-coded in slice):

- **EXPONER** — `EnterPlanMode`, `ExitPlanMode`, `ToolSearch`,
  `TodoWrite`, `SyntheticOutput`, `NotebookEdit`,
  `cron_create`/`list`/`delete`/`pause`/`resume`,
  `ListMcpResources`, `ReadMcpResource`,
  `config_changes_tail`, `web_search`, `web_fetch`.
- **NO-EXPONER** — `Heartbeat` (timer-only), `delegate`
  (a2a no-MCP-target), `RemoteTrigger` (binding context
  required).
- **DEFERRED** — `Lsp` (79.M.b — LspManager boot),
  `TeamCreate`/`TeamDelete`/`TeamSendMessage`/`TeamList`/
  `TeamStatus` (79.M.d — store + router boot),
  `Config` (`config-self-edit` Cargo feature; 79.M.c —
  full applier+correlator+auth_token wiring +
  security review).

**Code touchpoints**:
- `crates/config/src/types/mcp_exposable.rs` (NEW) — slice +
  `SecurityTier`, `BootKind`, `ExposableToolEntry`,
  `lookup_exposable`. 8 unit tests.
- `crates/core/src/agent/mcp_server_bridge/` (now a module
  dir) — `mod.rs` + new `context.rs`, `dispatch.rs`,
  `telemetry.rs`; legacy `ToolRegistryBridge` lives in
  `bridge.rs`. 23 new unit tests across bridge tree.
- `crates/core/src/agent/tool_registry.rs` — new
  `register_arc(def, handler)` accepts pre-boxed
  `Arc<dyn ToolHandler>`.
- `crates/core/src/telemetry.rs` — 2 new counters
  (`mcp_server_tool_registered_total{name,tier}`,
  `mcp_server_tool_skipped_total{name,reason}`).
- `src/main.rs::run_mcp_server` — match arm legacy
  replaced by `EXPOSABLE_TOOLS` loop with best-effort
  boot of `cron_store`, `config_changes_store`,
  `web_search_router` from env / disk.
- `crates/core/tests/exposable_catalog_test.rs` (NEW) —
  9 conformance tests covering catalog invariants +
  per-disposition boot semantics + Always round-trip
  shape contract.

**Done criteria**:
- `EXPOSABLE_TOOLS` covers every MVP exposable name with
  exactly one `ExposableToolEntry`. (`no_duplicate_names`
  test verifies.)
- Boot dispatcher returns the 6 expected `BootResult`
  variants — registered, denied, deferred, feature-gated,
  infra-missing, unknown — each with a labelled reason.
- Conformance suite verifies every `Always` entry boots
  with a full context and produces a tool def with a
  JSON-object schema.
- Operator typo (entry not in slice) emits a warn line
  with `expose_tools entry not in EXPOSABLE_TOOLS catalog`
  message.
- Telemetry counters render in `/metrics` even before any
  registration/skip event fires.

**Sub-pasos**:
- [x] 79.M.1 — slice + types + lookup (`mcp_exposable.rs`).
- [x] 79.M.2 — `McpServerBootContext` + builder.
- [x] 79.M.3 — `BootResult` + `boot_exposable` skeleton.
- [x] 79.M.4 — telemetry counters.
- [x] 79.M.5 — boot helpers for handle-free Always entries
      (6 tools).
- [x] 79.M.6 — boot helpers for `cron_*` (5 tools).
- [x] 79.M.7 — boot helpers for `mcp_router` (2 tools).
- [x] 79.M.8 — boot helper for `config_changes_tail`.
- [x] 79.M.9 — boot helpers for `web_search` + `web_fetch`.
- [x] 79.M.10 — refactor `run_mcp_server` to walk the slice.
- [x] 79.M.11 — conformance suite
      (`crates/core/tests/exposable_catalog_test.rs`).
- [x] 79.M.12 — docs + admin-ui + PHASES sync.

Deferred (follow-up sub-phases):
- **79.M.b** — `Lsp` exposure: `LspManager::boot()` reused
  in mcp-server mode (Phase 79.5 already encapsulates the
  helper).
- **79.M.c** — `Config` exposure: full applier + denylist
  bridge + `ApprovalCorrelator` + `auth_token_env` enforced
  + security review of MCP-driven self-edit attack surface.
- **79.M.d** — `Team*` exposure: `SqliteTeamStore` +
  `TeamMessageRouter` boot in mcp-server mode (Phase 79.6.b
  prerequisite).

#### 79.14 — docs + admin-ui sync   ✅

- `docs/src/agents/` gains pages for plan mode, tool
  search, synthetic output, todo write, LSP tool, team
  fanout, scheduled cron, remote triggers, brief mode,
  config self-edit (with the security caveats), MCP
  resource navigation, REPL, notebook editing.
- `admin-ui/PHASES.md` adds checkboxes for every operator-
  visible knob landed in 79.1–79.13.
- `crates/setup/src/capabilities.rs::INVENTORY` registers
  the new dangerous toggles: `repl.enabled`,
  `repl.allow_unsandboxed`, `config.self_edit`,
  `cron.user_max_entries` overrides,
  `plan_mode.auto_enter_on_destructive`,
  `lsp.enabled`, every `remote_triggers[*].name`
  (informational entry per allowlisted target).

##### Sequencing within Phase 79

Suggested order: 79.1 (plan mode — small, immediate UX
win) → 79.4 (todo write — small, drives 79.6/79.7 better)
→ 79.2 (tool search — unlocks wide-surface MCP) →
79.3 (synthetic output — wires Phase 51 eval) →
79.7 (cron — combines with 77.20 proactive) →
79.8 (remote trigger) → 79.9 (brief) → 79.11 (MCP
resources — Phase 12.5 follow-up) → 79.5 (LSP — bigger
crate, deserves room) → 79.6 (team fanout — depends on
77.18 coordinator) → 79.13 (notebook) → 79.12 (REPL —
sandbox is the biggest engineering chunk) → 79.10
(config self-edit — needs security review and goes last
on purpose) → 79.14 (docs sync).

##### Effort estimate

Aggregate ~3 engineer weeks. The expensive items are 79.5
LSP (3-4 days, new crate), 79.12 REPL (3 days, sandbox
infra), 79.6 Team (2-3 days, registry plumbing), and
79.10 Config (2 days + security review). Everything else
is ≤ 1 day each because the underlying machinery
(registry, dispatcher, audit log, hot-reload) already
exists.

### Phase 80 — KAIROS autonomous assistant mode parity   ⬜

KAIROS is the leak's flagship "always-on" agent surface. It
gates 8 distinct primitives behind GrowthBook flags and ties
them together with `assistantModule` + `kairosEnabled` runtime
state. Phase 77 / Phase 79 ported the *easy* halves
(proactive ticks, Sleep, ScheduleCron MVP, RemoteTrigger,
BriefTool spec). Phase 80 closes the remaining gaps so an
agent can run as a forked daemon, wake on GitHub webhooks,
push to mobile, consolidate memory while idle, and produce
an "away digest" when the operator returns.

This phase intentionally does NOT port: GrowthBook itself
(use Phase 18 hot-reload + binding policy instead), UDS
sockets (NATS subjects supersede), the cloud session-history
API (Phase 72 turn log is local + offline + already shipped),
the Anthropic-internal `tengu_*` flag namespace (we expose
the same knobs through `agents.yaml` + per-binding override).

Detailed surface analysis (with leak file + line + nexo
file + LOC count) lives in
`/home/familia/chat/proyecto/design-agent-framework.md`
under the "Phase 80 KAIROS port" appendix added in 80.0.

References (PRIMARY):
- `claude-code-leak/src/services/autoDream/{autoDream,consolidationLock,consolidationPrompt,config}.ts`
- `claude-code-leak/src/utils/{forkedAgent,cronScheduler,cronTasks,cronJitterConfig,concurrentSessions,conversationRecovery}.ts`
- `claude-code-leak/src/tools/{BriefTool,ScheduleCronTool,SubscribePRTool,PushNotificationTool,SleepTool}/`
- `claude-code-leak/src/services/mcp/channelNotification.ts`
- `claude-code-leak/src/services/extractMemories/extractMemories.ts:171-222` (canUseTool whitelist)
- `claude-code-leak/src/tasks/DreamTask/DreamTask.ts`
- `claude-code-leak/src/main.tsx:559,685,1058-1088,1075,2197-2208,2518,2916,3035,3259-3340,3832-3845,4334,4612-4625` (KAIROS integration points)
- `claude-code-leak/src/utils/messages/systemInit.ts:80-93` + `claude-code-leak/src/setup.ts:90-101` (UDS bootstrap shape — informs our NATS-subject contract, not a verbatim port)

References (secondary — current nexo state):
- `crates/core/src/agent/dreaming.rs` (515 LOC — scoring-based,
  NOT fork-based; 80.1 keeps scoring as light-pass and adds
  the fork as the deep pass)
- `crates/core/src/cron_runner.rs` (639 LOC — already has
  jitter_pct + retry; 80.2/80.3/80.4/80.5 add the 6-knob
  hot-reload, deterministic per-task jitter, one-shot lead
  semantics, `permanent` flag)
- `crates/core/src/agent/cron_tool.rs` (626 LOC — MVP from
  Phase 79.7; 80.4/80.5 wire the runtime firing path)
- `crates/core/src/agent/sleep_tool.rs` (244 LOC — Phase 77.20)
- `crates/driver-loop/src/proactive.rs` (66 LOC — Phase 77.20)
- `crates/driver-loop/src/extract_memories.rs` (1103 LOC —
  Phase 77.5; 80.20 reuses its tool-whitelist shape)
- `crates/agent-registry/` (Phase 67 + 71 — adds 80.10 SessionKind
  enum + 80.18 DreamRun row)
- `crates/core/src/team_message_router.rs` (365 LOC — Phase 8;
  80.11 adds an inbox subject contract on top)
- `crates/poller/src/builtins/webhook_poll.rs` (poll-only;
  80.12 adds the *receive* side as a sibling builtin)

#### 80.0 — KAIROS surface inventory + design appendix   ✅

Shipped — `proyecto/design-kairos-port.md` written end-to-end
(7 sections, ~600 lines). Linked from
`proyecto/design-agent-framework.md::Phase-specific design appendices`.

The appendix is the single source of truth for 80.1–80.21:
inventories every `claude-code-leak/` file each sub-phase
touches with (a) leak path:line, (b) nexo equivalent path:LOC,
(c) gap classification, (d) decisions log (D-1 .. D-10),
(e) per-sub-phase brainstorm-citation checklist enforcing the
UNBREAKABLE memory rule (every `/forge brainstorm` cites at
least one path:line from the leak AND from `research/`, or
explicitly states absence).

Decisions captured in the appendix that re-shape downstream
sub-phases:

- **D-1**: keep `dreaming.rs` scoring sweep alongside the
  forked /dream — scoring becomes the *light pass* per turn,
  fork is the *deep pass* every ≥ 24 h.
- **D-2**: NATS subjects (`agent.inbox.<goal_id>`) supersede
  UDS_INBOX entirely — multi-host, persistence, fan-out.
- **D-3**: no GrowthBook port — `tengu_*` flags map to
  per-binding YAML + Phase 18 hot-reload + `setup doctor`.
- **D-4**: `kairos_remote_control` is a dial *within* the
  Phase 16 capability gate, NOT a bypass — gate stays
  authoritative; mode only flips auto-approve dial inside
  what gate already permits.
- **D-7**: brief mode is a `SendUserMessage` gate, not a terse
  toggle — re-spec of Phase 79.9.
- **D-8**: forked subagent gains a `ForkAndForget` mode
  alongside the existing `Sync` delegation — autoDream +
  AWAY_SUMMARY use fire-and-forget.
- **D-9**: `assistant_mode: true` implies a default bundle
  (brief on, cron on, proactive on, auto-dream deep on,
  team auto-spawn on) — but `kairos_remote_control` stays
  off explicitly (D-4 hardening).
- **D-10**: 80.7 (per-cwd scheduler lock) deferred until
  Phase 32 multi-host orchestration.

#### 80.1 — autoDream fork-style consolidation   ✅ MVP

Shipped — `crates/dream/` foundation crate (~1200 LOC + 49 unit tests).
Verbatim port of leak `services/autoDream/autoDream.ts:1-324` +
`consolidationLock.ts:1-140` + `consolidationPrompt.ts:1-65`. Mirrors
the leak's per-turn-hook design (NOT cron-based) per the spec audit.

**Modules shipped**:

- `error.rs` — typed `AutoDreamError` enum (`Io`, `LockBlocked`,
  `Timeout`, `Fork`, `Audit`, `Config`).
- `config.rs` — `AutoDreamConfig` with leak-faithful defaults
  (`min_hours=24h`, `min_sessions=5`, `scan_interval=10min`,
  `holder_stale=1h`); `fork_timeout=5min` is a nexo addition (leak
  has no explicit fork timeout).
- `consolidation_lock.rs` — `ConsolidationLock` PID/mtime lock
  verbatim from leak `:1-140`. `mtime IS lastConsolidatedAt`,
  `HOLDER_STALE_MS=1h`, idempotent rollback, symlink defense via
  canonicalize-at-construction. `is_pid_running` via `nix::sys::signal::kill`.
  `list_sessions_touched_since` mirrors leak `:118-124` (UUID stem
  filter, exclude current).
- `consolidation_prompt.rs` — `ConsolidationPromptBuilder` verbatim
  port of leak `:10-65` 4-phase template (Orient → Gather →
  Consolidate → Prune). Constants `ENTRYPOINT_NAME=MEMORY.md`,
  `MAX_ENTRYPOINT_LINES=200`, `DIR_EXISTS_GUIDANCE` lifted from
  leak's `memdir.ts:34,35,116`.
- `dream_progress_watcher.rs` — `DreamProgressWatcher` impl
  `nexo_fork::OnMessage`. Verbatim port of leak's
  `makeDreamProgressWatcher` (`autoDream.ts:281-313`): per assistant
  message extracts text + counts tool_uses + extracts FileEdit/FileWrite
  `file_path` + canonicalize + appends to 80.18 `DreamRunStore` via
  `append_turn` + `append_files_touched`. Defense-in-depth: paths
  outside `memory_dir` recorded in `escapes` for post-fork audit
  (nexo addition).
- `auto_dream.rs` — `AutoDreamRunner` control flow. 7-gate sequence
  (disabled / kairos_active / remote_mode / auto_memory / time / scan
  throttle / sessions). Force path uses `priorMtime = lastAt` so
  rollback no-op (leak `:174-179`). Lock acquire blocked branch
  returns `RunOutcome::LockBlocked { holder_pid, mtime_secs }` (nexo
  structured outcome — leak returns `Promise<void>`). Fork via
  `nexo_fork::DefaultForkSubagent` with `AutoMemFilter` (80.20) +
  `DreamProgressWatcher` + 80.18 audit row insert/update/finalize.
  Post-fork escape audit returns `RunOutcome::EscapeAudit` if any
  edit-path canonical is outside `memory_dir`. `tracing::info!`
  events with leak field names (`hours_since`, `sessions_since`,
  `cache_read`, `cache_created`, `output`, `sessions_reviewed`).

**Three pillars verified**:

- **Robusto**: 49 unit tests; defense-in-depth (80.20 whitelist +
  per-call canonicalize + post-fork escape audit + lock); typed
  errors; idempotent rollback (test cubre `rollback(0)` + double
  call); symlink defense via canonicalize-at-construction; `Option<i64>`
  distinguishes `Some(0)` from `None` for `prior_mtime_ms`; gate-7
  ordering preserves leak's cheapest-first (one stat for time-gate
  before listdir for sessions).
- **Óptimo**: reuses 80.18 `DreamRunStore` + 80.19 `DefaultForkSubagent`
  + 80.20 `AutoMemFilter` + leak prompt template — no logic
  duplication; `Arc<ArcSwap<AutoDreamConfig>>` hot-reload friendly;
  `AtomicI64` for `last_session_scan_at` lock-free; lock mtime IS
  lastConsolidatedAt (one stat per turn, not separate state file);
  PID liveness via `nix::sys::signal::kill(pid, None)` cheap syscall.
- **Transversal**: `Arc<dyn LlmClient>` provider-neutral; `MockLlm` +
  `MockFork` test fixtures unblock cross-provider testing without
  Anthropic-specific assumptions; `fork_label: String` reusable for
  AWAY_SUMMARY (80.14) + future eval (Phase 51).

**Decisions taken** (validated by leak re-audit + spec amend):

- **NO heartbeat lock during fork** — leak doesn't have one;
  operator with `> 1h` forks tunes `holder_stale`. Drops 1 config
  field + 1 test from spec.
- **Per-turn hook, NOT cron** — leak `executeAutoDream` from
  `stopHooks`. Mirror Phase 77.5 `extract_memories` integration
  pattern in driver-loop (deferred to 80.1.b).
- **Watcher BIDIRECTIONAL** — collects turns AND files via 80.18
  store calls (verbatim leak `makeDreamProgressWatcher`).
- **Verbatim `extra` block text** — leak `:216-221` substituted
  with sessions list.
- **`tracing::info!` with leak field names** — `auto_dream.fired`,
  `auto_dream.completed`, `auto_dream.failed` targets; field names
  preserved sans `tengu_` prefix.
- **`RunOutcome` enum** — nexo addition for CLI/LLM-tool feedback.
- **Buffer pattern `_pending_promotions.md`** — design specified;
  implementation deferred to 80.1.e (D-1 coexistence with Phase
  10.6 scoring sweep).

**Files shipped**:

- `crates/dream/Cargo.toml`, `crates/dream/README.md`,
  `crates/dream/src/{lib,error,config,consolidation_lock,
  consolidation_prompt,dream_progress_watcher,auto_dream}.rs` (8 files)
- `Cargo.toml` workspace.members += `crates/dream`

**Tests** (49 unit, all verde):
- 2 — error display + From conversions
- 5 — config defaults match leak + validation
- 16 — consolidation_lock (read_last, try_acquire happy/blocked/dead-PID/stale,
  rollback unlink/utimes/idempotent, record_consolidation, holder_info,
  list_sessions filter + exclude current + non-UUID skip, missing dir, PID liveness)
- 8 — consolidation_prompt (4 phases rendered, paths substituted,
  entrypoint+max_lines, dir-exists guidance, extra block append/omit,
  override max_lines, summary footer)
- 7 — dream_progress_watcher (ignores user msgs, text-only turn,
  tool_use count, FileEdit inside memdir, escape outside memdir,
  ignores non-edit tools, dedupe paths)
- 11 — auto_dream control flow (build_extra, 7 gate-skip variants,
  force bypass, completed audit row, fork error rolls back, fork
  timeout records killed, lock blocked structured outcome)

**Follow-ups** (split as 80.1.b/c/d/e):

- **80.1.b.b.b.c per-goal_id multi-runner dispatch** ✅ **MVP** —
  `DriverOrchestrator::auto_dream` field swapped from
  `Mutex<Option<Arc<dyn AutoDreamHook>>>` to
  `Mutex<HashMap<String, Arc<dyn AutoDreamHook>>>` keyed by owning
  `agent_id`. Per-turn dispatcher reads the routing key from
  `goal.metadata["agent_id"]` (the canonical convention; populated
  via `Goal::with_agent_id` / read via `Goal::agent_id`) and looks
  it up in the map. New API surface: `register_auto_dream`
  (returns the displaced hook, if any), `unregister_auto_dream`,
  `auto_dream_agents` (sorted ids — stable for assertions),
  `has_auto_dream`. `DreamContext.agent_id` field added so
  downstream runners receive the routing key already resolved.
  Backward compat shim: `set_auto_dream(Option<...>)` from
  Phase 80.1.b.b.b.b stays available behind `#[deprecated]` and
  routes to the sentinel `"_default"` key (warn-once via
  `OnceLock`). Builder `.auto_dream(hook)` likewise registers
  under `"_default"`. Boot wire in `src/main.rs::Mode::Run` now
  iterates every active runner and registers each under its
  `agent_id`; single `tracing::info!` summary lists
  `agents = N, registered = [...]`. Empty agent_id with non-empty
  map → warn (the goal didn't declare its owner); unknown
  agent_id → debug (multi-tenant SaaS legitimately carries stale
  metadata). Verification: 5 integration tests in
  `tests/orchestrator_auto_dream_registry_test.rs` — register
  returns prev / overwrite returns prev / unregister returns
  prev-or-none / sorted keys / set_auto_dream compat shim routes
  to `"_default"`. 4 unit tests in `goal::tests` cover the
  `with_agent_id` / `agent_id()` helpers (presence, missing,
  wrong-shape, empty-string). Hot-reload integration with the
  Phase 18 reload loop, lifecycle event for admin-ui, and
  Prometheus gauge for `auto_dream_agents.len()` are deferred
  follow-ups.
- **80.1.b.b.b.b orchestrator runtime-attach** ✅ **MVP** —
  `DriverOrchestrator::auto_dream` field now wraps
  `Mutex<Option<Arc<dyn AutoDreamHook>>>` (was a plain `Option`).
  Pub setter `set_auto_dream(Option<...>)` lets the boot wire
  attach the runner after the orchestrator is constructed (the
  builder fires inside `boot_dispatch_ctx_if_enabled`, before
  the per-agent loop populates the runner Vec). Reads inside
  `run_turn` clone the Arc out of the lock so no await happens
  while the mutex is held. `arc_swap::ArcSwapOption` was the
  first attempt but it requires `T: Sized` and the trait object
  is unsized — stdlib `Mutex` is the lowest-friction wrap.
  Boot wire in `src/main.rs` picks the primary (first non-None)
  runner after the loop and calls
  `dispatch_ctx.orchestrator.set_auto_dream(Some(primary))`.
  When `dispatch_ctx` is `None` (no agent has
  dispatch_capability=full) the runner stays reachable via the
  `dream_now` LLM tool only — logged so operators see the
  state. Multi-runner routing remains a follow-up
  (`80.1.b.b.b.c — per-goal_id dispatch`); MVP picks the first
  runner with a single warn listing the skipped agent ids.
  Commit: `549828c`.
- **80.1.b.b.b consumer** ✅ **MVP** — `src/main.rs::Mode::Run`
  per-agent loop now constructs an `AutoDreamRunner` for every
  agent that has `auto_dream.enabled = true`. Wires the full
  constellation: `nexo_fork::AgentToolDispatcher` (new Phase
  80.1.b.b.b step A — see commit `a5be157`),
  `parent_ctx_template = AgentContext::new(...)`,
  `MemoryGitCheckpointer` over the per-agent `MemoryGitRepo`,
  and **`PreDreamSnapshotAdapter` over the shared snapshotter**
  when `memory.snapshot.auto_pre_dream = true` (closes MS-3).
  `dream_now` LLM tool registered per-agent when
  `NEXO_DREAM_NOW_ENABLED=true` and `transcripts_dir` non-empty.
  Per-agent tracing emit at boot
  (`auto_dream_enabled`, `has_pre_dream_snapshot`,
  `has_git_checkpointer`, `dream_now_registered`). Failure of
  any single agent's `BootDeps` → log warn + `None` runner;
  daemon proceeds. `nexo_fork::AgentToolDispatcher` lives in
  `nexo-fork` (not `nexo-core`) because nexo-fork already
  depends on nexo-core — putting the bridge in nexo-core would
  form a cycle. Boot wire commits: `5fe2cc0`. 2 new tests in
  `boot.rs::tests` covering the `pre_dream_snapshot` field
  threading. Out of scope: orchestrator
  `.auto_dream(primary)` registration — the
  `DriverOrchestrator::builder()` lives inside
  `boot_dispatch_ctx_if_enabled` which runs BEFORE the
  per-agent loop, so the runner Vec isn't populated when the
  builder fires. Tracked as Phase 80.1.b.b.b.b multi-runner +
  orchestrator wire follow-up. The runner is constructed and
  available to the `dream_now` LLM tool today; automatic
  per-turn dispatch via the orchestrator lands when the
  refactor lands.
- **80.1.b.b.b helper** ✅ **MVP** — shipped:
  `crates/dream/src/boot.rs` (~270 LOC + 7 unit tests). Operator
  calls `nexo_dream::boot::build_runner(BootDeps { ... })` once at
  startup; helper validates config, mkdirs memory_dir + state_root
  parent, opens `SqliteDreamRunStore` (shared `<state>/dream_runs.db`),
  constructs `ConsolidationLock`, builds `AutoDreamRunner` via
  `with_default_fork`. Returns `Ok(None)` when `enabled: false`
  (orchestrator stays clean — no per-turn cost). Public path
  helpers `default_memory_dir(workspace_root, agent_id)` +
  `default_dream_db_path(state_root)` documented as Phase 10.6 +
  80.18 conventions. Mirror leak `autoDream.ts:111-122`
  `initAutoDream()` startup pattern. Provider-agnostic via
  `Arc<dyn LlmClient>` + `Arc<dyn ToolDispatcher>` in `BootDeps`.
  Module doc comment includes the 3-line main.rs hookup snippet
  for operator-side application when their existing main.rs
  pre-existing dirty state (`CronToolCallsConfig` + `Arc` import)
  resolves. Tests verde: 1 path-helper composition + 6 build_runner
  integration (returns None disabled, validates config, creates
  default memory_dir, honors explicit override, creates state_root
  parent, returns Some when enabled). nexo-dream cumulative: 55
  tests verde (48 + 7).
- **80.1.b.b** — `AgentConfig::auto_dream` field + workspace
  fixture sweep ✅ **partial** — shipped: field added with
  `#[serde(default)]` to `crates/config/src/types/agents.rs`;
  47 struct-literal fixtures across 17 directories swept via
  `perl -i -p0e` multi-line replace (anchor `repl: Default::default(),\n}`);
  3 new YAML round-trip tests in `nexo-config::types::agents::auto_dream_yaml_tests`
  (without/with/disabled). All affected crates verde:
  nexo-config 153 tests, nexo-fork 66, nexo-dream 48,
  nexo-driver-loop 104, nexo-driver-types 22, nexo-agent-registry 38,
  nexo-core 671 unit + many integration. main.rs boot wiring + Phase 18
  hot-reload propagation deferred as **80.1.b.b.b** follow-up — main.rs
  is ~10K LOC with multiple AgentContext sites; needs investigative
  audit + careful per-binding plumbing of `parent_ctx_template` /
  `tool_dispatcher` / `dream_db_path`. The orchestrator integration
  shipped in 80.1.b is functional standalone — operators can wire
  programmatically; main.rs convenience layer is an enhancement.
- **80.1.b** — driver-loop post-turn hook integration ✅ **MVP** —
  shipped: `AutoDreamHook` trait + `AutoDreamOutcomeKind` enum +
  `DreamContextLite` struct in `nexo-driver-types::auto_dream`
  (places upstream of both nexo-driver-loop and nexo-dream to
  break the would-be cycle). `DriverOrchestrator` gains
  `auto_dream: Option<Arc<dyn AutoDreamHook>>` field + `.auto_dream(...)`
  builder method + invocation site adjacent to Phase 77.5
  (`crates/driver-loop/src/orchestrator.rs:728-756`). `nexo-dream`
  provides `impl AutoDreamHook for AutoDreamRunner` with
  `run_outcome_to_kind` mapping. `DriverEvent::AutoDreamOutcome
  { goal_id, outcome_kind }` variant. `DreamContext` refactored
  to drop `parent_ctx`+`last_chat_request`; `AutoDreamRunner::new`
  now accepts operator-supplied `parent_ctx_template` +
  `fork_system_prompt` + `fork_tools` + `fork_model` (mirror Phase
  77.5 ExtractMemories shape; no parent prompt-cache share).
  `AutoDreamConfig` moved to `nexo-config::types::dream` (avoids
  dep cycle); nexo-dream re-exports + adds `validate()` helper.
  Tests verde: nexo-dream 48, nexo-driver-types 1, nexo-driver-loop 104,
  nexo-config 150+. AgentConfig field + main.rs boot wiring split as
  **80.1.b.b** follow-up because adding `auto_dream` field to
  `AgentConfig` breaks 30+ struct-literal test fixtures across
  the workspace; needs a coordinated multi-crate sweep.
- **80.1.c** — `dream_now` LLM tool ✅ **MVP** — shipped:
  `crates/dream/src/tools.rs` (~250 LOC + 9 unit tests).
  `DreamNowTool { runner: Arc<AutoDreamRunner>, transcript_dir: PathBuf }`
  implements `ToolHandler::call(ctx, args)`: extracts optional
  `args.reason: string` (defensive — empty / missing / non-string
  collapse to `"no reason given"`), reads `ctx.session_id`
  (errors if missing because forced runs need a goal anchor),
  builds `DreamContext { goal_id, session_id, transcript_dir,
  kairos_active: false, remote_mode: false }`, calls
  `runner.run_forced(&ctx)` (bypasses kairos+remote+time+session
  gates per `AutoDreamRunner::run_forced` — lock gate still
  honored). `outcome_to_json` maps all 6 `RunOutcome` variants
  to a structured JSON response (`status: "completed" | "skipped" |
  "lock_blocked" | "errored" | "timed_out" | "escape_audit" |
  "force_completed"`, plus `reason`, `audit_run_id`, `files_touched`,
  `error_message` as applicable). `register_dream_now_tool(registry,
  runner, transcript_dir)` boot helper registers the tool via
  `register_arc`. `tool_def()` returns `ToolDef { name: "dream_now",
  description, parameters: { type: "object", properties: { reason: {
  type: "string", description: "Optional human-readable reason..." }},
  additionalProperties: false } }`. **Capability gate entry deferred**
  as 80.1.c.b — `crates/setup/src/capabilities.rs::INVENTORY` write
  needs the gate-id to align with Phase 16's binding-policy schema
  (gate is `dream_now` opt-in; default `deny` for non-`assistant_mode`
  bindings); standalone tool registration shipped + provider-agnostic
  contract verified (no Anthropic-specific types in public surface).
  Mirror leak: forced consolidation pattern from
  `claude-code-leak/src/services/autoDream/autoDream.ts:102-179`
  (`runAutoDream` callable directly when manual trigger present),
  Phase 77.20 Sleep tool shape (string param + JSON response).
  3-line main.rs hookup snippet (build runner via 80.1.b.b.b helper,
  call `register_dream_now_tool(&registry, runner, transcript_dir)`)
  documented in module doc comment for operator-side application.
  Tests verde: 9 inline (tool_def_shape, call_with_reason_returns_completed,
  call_without_reason_uses_default, call_with_empty_reason_uses_default,
  call_with_non_string_reason_uses_default, call_without_session_id_errors,
  outcome_to_json_skipped_renders_gate, register_dream_now_tool_adds_to_registry,
  outcome_to_json_lock_blocked_renders_holder_pid).
  nexo-dream cumulative: 64 tests verde (55 + 9).
- **80.1.c.b** — capability gate INVENTORY entry ✅ **MVP** —
  shipped: `crates/setup/src/capabilities.rs::INVENTORY` appends
  `CapabilityToggle { extension: "dream", env_var:
  "NEXO_DREAM_NOW_ENABLED", kind: Boolean, risk: Medium, effect:
  ..., hint: "export NEXO_DREAM_NOW_ENABLED=true" }` so
  `nexo setup doctor capabilities` lists the host-level dream_now
  gate. `crates/dream/src/tools.rs::register_dream_now_tool` now
  reads the env via `is_dream_now_env_enabled()` (mirror of
  `evaluate_one` Boolean coercion — `true`/`1`/`yes` truthy,
  case-insensitive, trimmed) and short-circuits with
  `tracing::info!("dream_now: host-level capability gate closed")`
  when unset / falsy. Two-layer gate: (1) host env (this entry,
  default deny) ∧ (2) Phase 16 per-binding `allowed_tools`
  (verified existing schema accepts arbitrary tool names — no
  schema change required). 7 nexo-setup capability tests verde
  including `inventory_has_expected_entries` extended with 3 new
  asserts (env var present, extension `"dream"`, risk `Medium`,
  kind `Boolean`); 12 nexo-dream tools tests verde adding 4 new
  (`register_dream_now_skips_when_env_disabled`,
  `register_dream_now_skips_when_env_garbage`,
  `register_dream_now_registers_for_truthy_variants` covering 6
  truthy variants, `register_dream_now_skips_for_falsy_variants`
  covering 6 falsy + edge variants). Pulled `anyhow` from dev-deps
  to `[dependencies]` in `crates/dream/Cargo.toml` (pre-existing
  drift — `tools.rs` lib code used `anyhow::Result` but workspace
  build only worked when nexo-dream was compiled with dev-deps).
  Provider-agnostic: env-var gate runs BEFORE LLM dispatch — works
  under any `Arc<dyn LlmClient>` impl. Mirror leak
  `claude-code-leak/src/services/autoDream/autoDream.ts:95-107`
  composed-flag `isGateOpen` (we collapse to a single env var
  because per-binding allow/deny lives in Phase 16). nexo-dream
  cumulative: 67 tests verde (64 + 3 new + replaces existing
  register_dream_now_tool_adds_to_registry which now sets the env
  before calling).
- **80.1.d** — `nexo agent dream {tail|status|kill}` CLI subcommand
  ✅ **MVP** — shipped: `src/main.rs::Mode::AgentDream(AgentDreamSubcommand)`
  + `enum AgentDreamSubcommand { Tail | Status | Kill }` + 4 parser
  arms + dispatch arm + 3 async run fns + 1 helper. **Tail**: opens
  `dream_runs.db?mode=ro` via `SqliteDreamRunStore::open`, calls
  `tail(n)` or `tail_for_goal(goal, n)`, renders markdown table or
  JSON. **Status**: parses uuid, calls `get(uuid)`, renders full
  row + last 5 turns. **Kill**: parses uuid, fetches row, returns
  early-noop when status already terminal (`Completed` / `Failed` /
  `Killed` / `LostOnRestart`), warn-and-bail with exit 2 when row
  is `Running` and `--force` is absent, otherwise calls
  `update_status(Killed)` + `finalize(now())` and (when
  `--memory-dir` provided) `ConsolidationLock::rollback(prior_mtime)`.
  Path resolution 3-tier (`--db` > `NEXO_STATE_ROOT` env > XDG
  default `~/.local/share/nexo/state/dream_runs.db`); YAML tier
  intentionally absent for now since `agents.state_root` does not
  exist as a config field (state_root flows into `BootDeps`
  directly per 80.1.b.b.b). `DreamRunRow` gained `Serialize +
  Deserialize` derives so the `--json` path can `serde_json::
  to_string_pretty(&rows)` without an intermediate type. Workspace
  Cargo.toml gained `nexo-dream` + `nexo-driver-types` + `dirs = "5"`
  in `[dependencies]` and `tempfile = "3"` in `[dev-dependencies]`
  (rust-analyzer-flagged drift left over from Phase 80.1.c —
  M8.a CHANGELOG entry called this out as a binary-build blocker;
  this entry resolves it). 11 inline tests in `src/main.rs::tests`:
  `resolve_dream_db_path_override_wins`,
  `resolve_dream_db_path_uses_env_when_no_override`,
  `short_uuid_takes_first_eight_chars`,
  `run_agent_dream_tail_empty_db_exits_zero`,
  `run_agent_dream_tail_with_rows_renders`,
  `run_agent_dream_tail_json_output`,
  `run_agent_dream_status_not_found_errors`,
  `run_agent_dream_status_returns_row`,
  `run_agent_dream_status_invalid_uuid_errors`,
  `run_agent_dream_kill_already_terminal_is_noop`,
  `run_agent_dream_kill_running_with_force_flips_status`. All 11
  verde. CLI smoke: `NEXO_STATE_ROOT=/tmp/nexo-test/state ./target/
  debug/nexo agent dream tail` → "(no dream runs recorded yet — db
  not found at /tmp/nexo-test/state/dream_runs.db)" exit 0;
  `agent dream tail --json` → `[]` exit 0; `agent dream status
  <bogus-uuid>` → exit 1 "dream_runs DB not found". Mirror leak
  `claude-code-leak/src/components/tasks/BackgroundTasksDialog.tsx:281,315-317`
  `DreamTask.kill(taskId, setAppState)` semantics, but as CLI
  rather than Ink UI keyboard since nexo has no Ink-equivalent.
  Provider-agnostic by construction (pure SQLite + filesystem;
  zero LLM-provider touchpoints). Remaining 80.1.d.b (live abort
  via NATS subject contract — needs Phase 80.11 inbox), 80.1.d.c
  (`agent dream now` operator force trigger — needs daemon-runtime
  tool dispatch plumbing), parser unit tests deferred (covered by
  manual smoke + run-fn tests).
- **80.1.e** — Coordination skip (scoring sweep ↔ fork-pass) ✅
  **MVP** — shipped: **PIVOTED** del plan original "buffer pattern
  `_pending_promotions.md`" al **SKIP pattern** alineado con leak
  `extractMemories.ts:121-148` `hasMemoryWritesSince`. El buffer
  original era complejidad inventada (drain ordering, secret guard
  scoping sobre buffer, race en archivo de buffer); el leak resuelve
  el race entre dos memory-writers haciendo SKIP, no buffer. Cero
  archivos artifact en memory_dir, cero drain logic, cero serde
  churn. Nuevo trait `nexo_driver_types::ConsolidationLockProbe`
  (`crates/driver-types/src/consolidation_lock_probe.rs`, ~30 LOC
  + 1 trait-object-safety test) sentado upstream de `nexo-dream` y
  `nexo-core` (mismo cycle-break que 80.1.b `AutoDreamHook` y 80.1.g
  `MemoryCheckpointer`). Método `is_live_holder(&self) -> bool` SYNC
  (no async — es un stat + parse + kill(0), no surprise I/O); fail-
  open documentado. Impl en `crates/dream/src/consolidation_lock.rs`
  para `ConsolidationLock` (~15 LOC + 5 inline tests:
  `probe_returns_false_when_lock_absent`,
  `probe_returns_false_for_pid_zero` (rollback marker),
  `probe_returns_true_for_live_pid` (usa `std::process::id()` para
  evitar surpresas de PID 1 en sandbox), `probe_returns_false_for_dead_pid`
  (PID 999999 fuera del pid_max típico),
  `probe_returns_false_for_garbage_body`). Lectura del lock-file
  con `std::fs::read_to_string`, parse del PID body, `is_pid_running`
  reusing existing fn at `consolidation_lock.rs:217`. Cualquier I/O
  o parse error → `false` (fail-open). En `nexo-core::agent::dreaming`,
  `DreamReport` gana field `deferred_for_fork: bool` y `DreamEngine`
  gana field `consolidation_probe: Option<Arc<dyn ConsolidationLockProbe>>`
  + builder `with_consolidation_probe(probe)`. `run_sweep` chequea
  el probe AL INICIO (después del log "dream sweep started", antes
  de cualquier query a SQLite) — si `probe.is_live_holder() == true`,
  retorna early con `DreamReport { deferred_for_fork: true,
  candidates_considered: 0, promoted: vec![], skipped_already_promoted: 0,
  ... }` y log info "dream sweep deferred — autoDream fork holds
  consolidation lock". Sin probe → behaviour idéntico a pre-80.1.e
  (preservación de compatibilidad backward). Trade-off documentado:
  promociones del scoring sweep durante la ventana del fork se
  difieren al siguiente turno — memorias hot siguen scoring high
  next turn, costo es a lo sumo un turno de latencia, mucho menor
  que la complejidad del buffer. 3 nuevos tests en
  `nexo-core::agent::dreaming::tests`:
  `run_sweep_proceeds_when_no_probe_configured` (probe `None` →
  promotion normal),
  `run_sweep_proceeds_when_probe_says_dead` (probe `Some` con
  `MockProbe::new(false)` → promotion normal, `deferred_for_fork:
  false`),
  `run_sweep_skips_when_probe_says_live` (probe `Some` con
  `MockProbe::new(true)` → `deferred_for_fork: true`, sin
  candidates considered, sin MEMORY.md, SQLite ledger sin
  promotion entry — verifica que NADA se escribió). Mock probe
  con `AtomicBool` toggleable para tests deterministas. Tests
  totales verde: nexo-driver-types 24 (23 + 1 nuevo), nexo-dream
  consolidation_lock 5 nuevos (72 totales), nexo-core dreaming 8
  (5 existing + 3 nuevos), 67+ nexo-dream tests siguen verde,
  workspace build verde. Provider-agnostic: pure filesystem +
  POSIX PID semantics; cero touchpoints LLM-provider; transversal
  Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI / Mistral.
  Defense-in-depth preservada: AutoMemFilter (Phase 80.20) ∧
  ConsolidationLock ∧ secret guard de Phase 77.7 ∧ MAX_COMMIT_FILE_BYTES
  + ahora también la coordination skip que evita race en MEMORY.md
  writes. main.rs hookup para construir
  `Arc::new(ConsolidationLock::new(memory_dir, holder_stale)) as Arc<dyn
  ConsolidationLockProbe>` cuando `dreaming.enabled && auto_dream.is_some()`
  documentado en doc-comment del builder — diferido hasta resolución
  de dirty state pre-existente. Out of scope deferred: 80.1.e.b
  (revivir buffer pattern si aparece evidencia de que el SKIP pierde
  promotions importantes), 80.1.e.c (sweep-during-fork via parallel
  write a archivo distinto). Mirror leak path:line directo:
  `claude-code-leak/src/services/extractMemories/extractMemories.ts:121-148`
  `hasMemoryWritesSince` SKIP pattern adaptado a lock-based variant.
- **80.1.g** — wire git auto-commit a AutoDream fork-pass ✅ **MVP**
  — shipped: nuevo trait `nexo_driver_types::MemoryCheckpointer`
  (`crates/driver-types/src/memory_checkpoint.rs`, ~25 LOC + 1
  trait-object-safety test) sentado upstream de `nexo-dream` y
  `nexo-core` (mismo cycle-break que el `AutoDreamHook` de Phase
  80.1.b). Adapter `MemoryGitCheckpointer { repo: Arc<MemoryGitRepo> }`
  en `crates/core/src/agent/workspace_git.rs` (~25 LOC + 2 tests
  `checkpointer_async_calls_commit_all` y
  `checkpointer_returns_ok_on_clean_worktree`) envuelve el
  `commit_all` blocking en `tokio::task::spawn_blocking`; orphan
  rule de Rust forzó newtype porque `impl ForeignTrait for
  Arc<Local>` no compila. `AutoDreamRunner` gana field
  `git_checkpointer: Option<Arc<dyn MemoryCheckpointer>>` + builder
  `with_git_checkpointer(ckpt)` + observability accessor
  `has_git_checkpointer()`. `run` invoca el checkpointer DESPUÉS
  de `audit.update_status(Completed) + finalize` y SOLO cuando
  `progress.touched.is_empty() == false` (D-2 — empty touches no
  generan commits vacíos; el audit row en `dream_runs.db` queda
  como única fuente de verdad). Helper `build_checkpoint_body(run_id,
  files)` formato `audit_run_id: <uuid>\n\n- path1\n- path2\n` para
  `git log --grep auto_dream` cross-link al audit row. Failure del
  checkpointer → `tracing::warn!` SIN downgrade del outcome
  (forensics es bonus, no bloqueante). `BootDeps` gana field
  `git_checkpointer: Option<Arc<dyn MemoryCheckpointer>>`,
  `boot::build_runner` lo cablea con `with_git_checkpointer(ckpt)`
  y emite `git_checkpoint_wired` en el log de boot. main.rs hookup
  para construir `MemoryGitCheckpointer::new(Arc::clone(&agent_git))
  as Arc<dyn MemoryCheckpointer>` queda documentado en doc-comment
  de `auto_dream.rs::with_git_checkpointer` — diferido hasta que
  el usuario resuelva su dirty state pre-existente con la hookup
  de `nexo_dream::boot::build_runner` general. 4 nuevos tests en
  `nexo-dream::auto_dream::tests`: `build_checkpoint_body_renders_run_id_and_paths`,
  `build_checkpoint_body_renders_empty_file_list`,
  `with_git_checkpointer_setter_round_trips`,
  `checkpoint_skipped_when_files_touched_empty` (verifica el guard
  `if !empty`),
  `checkpoint_failure_does_not_downgrade_completed_outcome`
  (verifica que `Err` del checkpointer no se propaga como Errored).
  `RecordingCheckpointer` mock impl con `AtomicUsize` counter y
  modo `failing()` para tests defensivos. Tests verde:
  nexo-driver-types 1 nuevo (23 total), nexo-core 2 nuevos
  (`workspace_git::tests::checkpointer_*`), nexo-dream 16 en
  `auto_dream` (12 + 4 nuevos) + 4 en `boot` (todos con
  `git_checkpointer: None` en mk_deps fixture). 67 tests nexo-dream
  totales verde. Mirror leak: NO hay precedente de
  `autoDream → git commit` en `claude-code-leak/` —
  `memdir/paths.ts:14` usa git solo para localizar memory dir;
  `memoryTypes.ts:187` documenta explícitamente que el leak NO
  duplica info git en memoria. Phase 10.9 git-backed memory es
  innovación nexo-específica; este sub-phase extiende esa parity
  al fork-pass deep consolidation. **Provider-agnostic**: trait
  permite cualquier checkpointer (git, S3, dual-write audit log);
  zero touchpoints LLM-provider; pure infra layer. Defense-in-depth
  preservada: AutoMemFilter (Phase 80.20 sandbox físico) ∧
  ConsolidationLock ∧ secret guard de Phase 77.7 (transparent vía
  `MemoryGitRepo::with_guard`) ∧ MAX_COMMIT_FILE_BYTES (1 MB) ∧
  `Mutex<Repository>` serialización. Out of scope (deferred):
  80.1.g.b commit on Killed con subject `KILLED` (revisar cuando
  haya demanda), 80.1.d.d auto `git revert HEAD` en kill.
- **80.1.f** — docs sweep autoDream cluster ✅ **MVP** — shipped:
  extendido `docs/src/soul/dreaming.md` (no nueva página, single
  point of truth para consolidation). 7 nuevas secciones append
  ~370 LOC: (1) **Two-tier consolidation: light + deep** con tabla
  comparativa (crate / cadence / cost / writes / failure mode /
  coordination / mirror leak); (2) **Deep pass via fork** con 7
  gates (kairos / remote / auto_memory / auto_dream / time / scan-
  throttle / session) + ConsolidationLock semantics (mtime IS
  lastConsolidatedAt + holder_stale 1h + canonicalize symlink
  defense) + 4-phase prompt (Orient → Gather → Consolidate → Prune)
  + AutoMemFilter restricciones (FileRead/Glob/Grep/REPL libres,
  Bash via `is_read_only`, FileEdit/Write scoped a memory_dir) +
  post-fork escape audit + MAX_TURNS=30 cap; (3) **Coordination:
  skip pattern** explicando que cuando ambos pases están enabled,
  el light pass chequea probe al inicio y defiere con
  `deferred_for_fork: true` cuando lock held; trade-off documentado;
  mirror leak `extractMemories.ts:121-148`; (4) **Audit trail** con
  schema completa de `dream_runs` SQLite table (id / goal_id /
  status / phase / sessions_reviewing / prior_mtime_ms /
  files_touched JSON / turns JSON / started_at / ended_at /
  fork_label / fork_run_id) + git commits con subject `auto_dream:
  N file(s) consolidated` y body con `audit_run_id` + cross-link
  example `git log --grep auto_dream | nexo agent dream status`;
  (5) **Operator CLI** con 3 sub-comandos `tail|status|kill` +
  4-5 ejemplos cada uno (incl. `--json`, `--goal`, `--n`, `--force`,
  `--memory-dir`, `--db`) + 3-tier path resolution `--db > NEXO_STATE_ROOT
  > XDG default`; (6) **LLM tool dream_now** con JSON tool shape
  + JSON envelope output (6 outcomes: completed / skipped / lock_blocked
  / errored / timed_out / escape_audit) + capability gate two-layer
  (host env `NEXO_DREAM_NOW_ENABLED=true` ∧ Phase 16 `allowed_tools`
  binding policy) + ejemplo de `nexo setup doctor capabilities`
  output; (7) **Configuration** con yaml block ejemplo + boot log
  output + asimetría documentada (auto_dream off no afecta light,
  dreaming off no afecta deep). Sección final **See also** con
  cross-links a Phase 10.9 / 18 / 77.7 / 80.18 / 80.20 + 5 leak
  paths con line ranges (autoDream.ts / consolidationLock.ts /
  consolidationPrompt.ts / extractMemories.ts / BackgroundTasksDialog.tsx).
  `mdbook build docs` smoke verde — sin broken links. `docs/src/SUMMARY.md`
  link `[Dreaming](./soul/dreaming.md)` ya existente, no requirió
  cambio. Página final ~560 LOC (era 186, +370). admin-ui panel +
  separate `concepts/kairos-mode.md` / `operations/cron-jitter.md`
  pages quedan reservadas para 80.21 (broader Phase 80 docs sweep,
  no cluster 80.1.x). OpenClaw
  `research/docs/concepts/dreaming.md` proveyó plantilla estructural
  (tabla phase model) sin copiar texto.
  Provider-agnostic: zero LLM-provider en ejemplos; todo
  el flow funciona bajo Anthropic / MiniMax / OpenAI / Gemini /
  DeepSeek / xAI / Mistral. Cluster 80.1.x core ahora cerrado
  (todas las sub-fases 80.1.x ✅ MVP except 80.1.d.b/c
  follow-ups).

Today `dreaming.rs` is a deterministic scoring sweep that
promotes high-recall memories to `MEMORY.md`. KAIROS's
autoDream is a *forked subagent* that grep-scans transcript
JSONL files, reads the existing memory dir, rewrites
top-level `*.md` files, and prunes the index. The two are
complementary: the scoring sweep is the cheap "light pass"
that runs every turn; the fork is the expensive "deep pass"
that runs once per ≥24 h when ≥5 sessions accumulated.

**Reference (PRIMARY)**: `claude-code-leak/src/services/autoDream/autoDream.ts:1-324`
+ `consolidationLock.ts:1-140`
+ `consolidationPrompt.ts:1-65`
+ `config.ts:1-22`
+ `claude-code-leak/src/utils/forkedAgent.ts` (`runForkedAgent`,
  `createCacheSafeParams`, `skipTranscript:true` semantics).
**Reference (secondary)**: extend `crates/core/src/agent/dreaming.rs`
with a new `deep_pass_via_fork()` entry point; reuses
`crates/memory/` write paths and the SQLite promotion ledger
already there for idempotency.

**Control flow** (mirror `autoDream.ts:isGateOpen` →
`runAutoDream` → completion / failure):

1. **Gates** (cheapest first):
   `kairos_active == false` (KAIROS uses disk-skill `/dream`
   instead — skip when KAIROS active to avoid double-fire)
   ∧ `is_remote_mode() == false`
   ∧ `is_auto_memory_enabled() == true`
   ∧ `is_auto_dream_enabled()` (binding flag + `agents.yaml`
   `dreaming.deep.enabled`).
2. **Time gate**: `hours_since(read_last_consolidated_at()) ≥ min_hours` (default 24).
3. **Scan throttle**: bail if `last_session_scan_at` was
   `< 10 min ago` AND time gate passed (avoids re-stat
   stamping on every turn — `autoDream.ts:55-58`).
4. **Session gate**: `list_sessions_touched_since(last_at)`
   excluding current → require ≥ `min_sessions` (default 5).
5. **Lock acquire**: `try_acquire_consolidation_lock()`
   returns prior mtime or `None` if blocked by live PID.
6. **Fork**: spawn forked goal with cache-safe params +
   `skip_transcript: true` + `can_use_tool` whitelist (80.20)
   + `consolidation_prompt`.
7. **Completion**: `complete_dream_task()`, append a system
   message "Improved N memory files" if `files_touched.len() > 0`.
8. **Failure**: if `abort_signal.is_aborted()` → user kill
   path, lock already rolled back by `DreamTask::kill()`.
   Else: `fail_dream_task()` + `rollback_consolidation_lock(prior_mtime)`.

**ConsolidationLock** (Rust port of `consolidationLock.ts`):
- File: `<memory_dir>/.consolidate-lock`
- mtime IS `lastConsolidatedAt` (one stat per turn — cheap)
- Body is the holder's PID
- Stale if PID dead OR `now - mtime ≥ 1 h` (HOLDER_STALE_MS)
- `try_acquire`: write PID, re-read, return prior mtime or
  `None` if lost the race / blocked by live PID
- `rollback(prior)`: if prior == 0 → `unlink`; else clear
  body + `utimes(prior)`. Idempotent — `kill()` rolling back
  twice is safe because second call sees mtime already at
  prior

**ConsolidationPrompt** (verbatim port of
`consolidationPrompt.ts:10-65`'s 4-phase prompt — orient,
gather, consolidate, prune; index ≤ 25 KB / `MAX_ENTRYPOINT_LINES`).
Add `extra` block with read-only Bash constraint reminder
(matches `autoDream.ts:216-221`).

**Done criteria**:
- New crate `crates/dream/` (or sub-module under `crates/core`)
  with `auto_dream`, `consolidation_lock`, `consolidation_prompt`
  modules
- Lock file lives at `<auto_mem_path>/.consolidate-lock`
- 4 unit tests: gate ordering, lock-mtime-is-last-consolidated,
  rollback idempotent, scan throttle blocks repeat fires
- 1 integration test: two parallel `try_acquire` calls →
  exactly one wins, the other gets `None`
- Hooked into `cron_runner` via a built-in entry that fires
  every `min_hours / 4` (gate handles real cadence)

#### 80.2 — Cron jitter 6-knob hot-reload config   ✅ MVP

Shipped together with 80.3-80.6 as the cron jitter cluster.

- `crates/config/src/types/cron_jitter.rs` — `CronJitterConfig`
  with the six knobs (`enabled`, `recurring_frac`,
  `recurring_cap_ms`, `one_shot_max_ms`, `one_shot_floor_ms`,
  `one_shot_minute_mod`, plus the recurring auto-expiry
  `recurring_max_age_ms`). `#[serde(default)]` keeps existing
  YAML rolling forward; `validate()` rejects out-of-range knobs
  at boot.
- `from_legacy_pct(pct)` shim keeps `CronRunner::with_jitter_pct`
  callers working without YAML changes — `pct/100.0` maps to
  `recurring_frac`.
- `CronRunner` now holds an `Arc<ArcSwap<CronJitterConfig>>` so a
  Phase 18 reload swaps the config atomically and the running
  tick observes the new value on the next read.
- 8 unit tests on the schema + 1 test on the runner killswitch.



Today `CronRunner::with_jitter_pct(pct)` is a single static
percentage. KAIROS exposes 6 knobs through
`tengu_kairos_cron_config` refreshed every 60 s with Zod
validation. Operators need the same 6 levers as an incident
shed-load tool.

**Reference (PRIMARY)**: `claude-code-leak/src/utils/cronJitterConfig.ts:1-78`
(the Zod schema + refresh interval) + `cronTasks.ts:286-333`
(`CronJitterConfig` defaults).
**Reference (secondary)**: `crates/core/src/cron_runner.rs:184`
(`with_jitter_pct`) + `crates/config/src/types/` (add new
`CronJitterYaml` block) + Phase 18 hot-reload watcher.

**Knobs** (Rust port — `serde` validation, sane defaults):
```rust
pub struct CronJitterConfig {
    pub recurring_frac: f32,        // [0.0, 1.0]
    pub recurring_cap_ms: u64,      // ≤ 30 min
    pub one_shot_max_ms: u64,       // ≤ 30 min
    pub one_shot_floor_ms: u64,     // ≤ one_shot_max_ms
    pub one_shot_minute_mod: u8,    // [1, 60]
    pub recurring_max_age_ms: u64,  // ≤ 30 days
}
```
Validation rejects whole config on any out-of-range value
(falls back to defaults — defense-in-depth against a bad
hot-reload).

**Done criteria**:
- `agents.yaml::cron.jitter` block accepts 6 knobs
- ConfigReloadCoordinator pushes new config into `CronRunner`
  via `ArcSwap<CronJitterConfig>` so the next tick reads the
  refreshed value (no restart needed)
- Bad config logs `WARN` and keeps prior config
- 3 unit tests: defaults, valid override, invalid override
  rejected wholesale

#### 80.3 — Cron task-id-derived deterministic jitter   ✅ MVP

`jitter_frac_from_entry_id(entry_id)` consumes the first 8 hex
chars of the (UUID) entry id and maps them to `[0.0, 1.0)` via
`u32::from_str_radix(...) / (u32::MAX as f64 + 1.0)`. Because the
id is stable across retries, a flapping entry lands on the same
offset within its window every time — operators reading
`next_fire_at` for a given row see a stable target instead of
chasing a moving timestamp.

Edge cases covered: short id, non-hex id (returns `0.0`,
collapses to "no jitter" rather than panic), id with separators.



Today jitter is RNG-based (different per fire). Per
`cronTasks.ts:381-398`, KAIROS derives the jitter fraction
deterministically from the task-id:
`jitterFrac(taskId) = parseInt(taskId.slice(0,8), 16) / 0x1_0000_0000`.
This way a retry doesn't move the firing target — important
when an operator is using cron jitter as a herd-shed
mechanism.

**Reference (PRIMARY)**: `claude-code-leak/src/utils/cronTasks.ts:381-398`.
**Reference (secondary)**: `crates/core/src/cron_runner.rs::apply_jitter`
(rewrite to take `entry_id: &str`).

**Done criteria**:
- `apply_jitter(next_fire, now, &entry.id, &cfg)` — signature
  change ripple
- Same `entry.id` always returns the same jitter for the same
  `(next_fire, cfg)`
- Property test: 1000 random entry-ids → uniform distribution
  over `[0, recurring_cap_ms)`

#### 80.4 — Cron one-shot vs recurring jitter modes   ✅ MVP

Two pure helpers in `crates/core/src/cron_schedule.rs`:

- `apply_recurring_jitter(next, following, from, entry_id, cfg)`
  — forward jitter `t1 + min(frac * (t2 - t1), cap_ms)`. Clamps
  to `from_unix + 1` when the offset would land in the past
  (e.g. when the runner ticks while a fire is already due).
- `apply_one_shot_lead(target, from, entry_id, cfg, target_minute)`
  — backward lead `target - max(frac * max_ms, floor_ms)`, gated
  by `target_minute % cfg.one_shot_minute_mod == 0`.
  `one_shot_minute_mod = 0` is the documented "never jitter
  one-shots" sentinel and short-circuits before the modulo.

Both helpers no-op when `cfg.enabled == false` so the killswitch
also dampens jitter-only behaviour.



Per `cronTasks.ts:421-445`: recurring jobs use **forward**
jitter `t1 + min(frac * (t2-t1), cap_ms)` (after natural fire
time, jitter capped). One-shot jobs use **backward lead**:
`max(t1 - lead, fromMs)` only if
`t1.minute() % one_shot_minute_mod == 0` (default mod 30 →
only :00 / :30 boundaries). Lead range:
`[one_shot_floor_ms, one_shot_max_ms)`.

**Reference (PRIMARY)**: `claude-code-leak/src/utils/cronTasks.ts:381-445`.
**Reference (secondary)**: `crates/core/src/cron_runner.rs`
(branch on `entry.recurring`).

**Done criteria**:
- Recurring path uses `min(frac * delta, cap)` forward
- One-shot path: skip when minute % mod ≠ 0; otherwise
  uniform lead in `[floor, max)` clamped to `entry.created_at`
- Unit tests: pinned-date recurring (t2 == None) → no jitter;
  one-shot at :15 → no jitter; one-shot at :30 → uniform lead

#### 80.5 — Cron `permanent` flag + `recurringMaxAgeMs`   ✅ MVP

- `CronEntry` gains a `permanent: bool` column with
  `#[serde(default)]` (false). Idempotent
  `ALTER TABLE ... ADD COLUMN permanent INTEGER NOT NULL DEFAULT 0`
  on boot, mirroring the existing `recipient` / `model_provider`
  migrations.
- `CronStore::sweep_expired_recurring(now, max_age_ms)` deletes
  recurring rows older than `max_age_ms` while exempting
  `permanent: true`. Honours `max_age_ms == 0` as "auto-expiry
  disabled" so the default config is a no-op.
- One-shot rows are never auto-expired by this sweep — the retry
  policy is the boundary that decides whether a one-shot is
  abandoned.

3 unit tests cover the recurring delete, permanent exemption,
one-shot preservation; plus a column round-trip test through
`SqliteCronStore`.



Built-in cron tasks (assistant mode catch-up,
morning-checkin, dream) need exemption from
`recurring_max_age_ms` auto-expiry. KAIROS gates this with
a `permanent: true` flag. Without it, an idle daemon's
built-ins would silently disappear after 30 days.

**Reference (PRIMARY)**: `claude-code-leak/src/utils/cronTasks.ts`
(`permanent` field + auto-expiry sweep).
**Reference (secondary)**: extend `nexo_core::cron::CronEntry`.

**Done criteria**:
- `CronEntry::permanent: bool` (defaults `false`)
- Auto-expiry sweep (`prune_old_entries`) skips
  `permanent: true` rows
- Setup wizard registers built-ins with `permanent: true`
- Unit test: 31-day-old `permanent=true` entry survives sweep

#### 80.6 — Cron killswitch + missed-task surfacing   ✅ MVP

- `CronRunner::tick_once` reads `cfg.enabled` at the top of each
  tick. When `false` the loop short-circuits before
  `due_at(...)` so paused entries stay durable in storage and
  resume on the next `true` tick. `tracing::debug!` traces the
  event for operator visibility.
- `CronStore::sweep_missed_entries(now, skew_ms)` rewrites
  `next_fire_at` to `i64::MAX` for every entry whose stored
  `next_fire_at` is older than `now - skew_ms`. `permanent: true`
  rows are exempt and `skew_ms == 0` is a no-op. Boot can call
  this once (after migrations, before the first tick) so a
  long-down daemon does not re-fire a stampede of "missed"
  entries — the operator sees them in `cron list` and can resume
  manually.
- 3 unit tests on the sweep + 1 on the killswitch round-trip
  (off → preserved → on → fires once).

**Boot-side wire-up (deferred)**: the daemon entrypoint needs to
call `store.sweep_missed_entries(...)` on startup with the
operator-configured `skew_ms`. That hookup follows once the
user-side dirty state on `src/main.rs` resolves — the helper is
ready to call.



Today CronRunner runs unconditionally as long as the daemon
is up. KAIROS polls a kill-switch every tick
(`isKilled?.()` + `tengu_kairos_cron` GrowthBook gate
refreshed every 5 min). Flip-off stops in-flight schedulers
mid-session, not just on next process boot. Plus, on initial
load only, `findMissedTasks(tasks, now)` surfaces one-shots
that should have fired while the daemon was offline; atomic
`nextFireAt[id] = Infinity` prevents double-fire on the
post-load tick.

**Reference (PRIMARY)**: `claude-code-leak/src/utils/cronScheduler.ts:230-260`
(killswitch poll inside `check()`) + `cronTasks.ts:193-227`
(`findMissedTasks`).
**Reference (secondary)**: `crates/core/src/cron_runner.rs::tick_once`.

**Done criteria**:
- `CronRunner` reads `enabled: bool` flag from
  `Arc<RuntimeSnapshot>` each tick — `false` = no fires
- Per-binding override `agents.<id>.cron.enabled: false`
- On `CronRunner::run` start: scan store for one-shots
  whose `next_fire < now - SAFETY_MARGIN`, surface via
  `notify_origin` "[catch-up] N tasks missed while offline",
  set `next_fire = i64::MAX` atomically
- Integration test: 3 missed one-shots after a simulated
  3-hour offline window → 1 catch-up notification, no
  double-fire

#### 80.7 — Cron scheduler per-cwd lock owner (multi-instance)   ⬜

Today multiple `nexo` daemons sharing the same
`agents.yaml` would double-fire cron entries. KAIROS uses a
`.scheduler-lock` per cwd — only the lock owner runs
`check()`, non-owners poll every 5 s for takeover. Less
critical for single-daemon deployments but mandatory before
we ship Phase 32 multi-host orchestration.

**Reference (PRIMARY)**: `claude-code-leak/src/utils/cronScheduler.ts:406-436`
(lock acquire / takeover / PID liveness).
**Reference (secondary)**: extend `crates/core/src/cron_runner.rs`
or factor into `crates/resilience/src/lock.rs` (advisory
file lock — `fs2` crate already in deps).

**Done criteria**:
- Lock file at `<cron_store_dir>/.scheduler-lock` with PID
  body
- Non-owner polls 5 s; takes over when PID dead or stale
  (mtime > 30 min)
- Lock released on graceful shutdown (Phase 71 SIGTERM
  drain hook)
- Integration test: spawn 2 runners → only one fires, kill
  the leader → the other takes over within 10 s

**Note**: low-priority — flag as DEFERRED until Phase 32
multi-host work needs it. Listed here for completeness.

#### 80.8 — Brief mode + `send_user_message` tool (re-spec of 79.9)   ✅ MVP

Brief mode tells the model that user-visible output flows through
the new `send_user_message` tool, not free text. Free text remains
visible in the detail view (no hide-filter yet — deferred 80.8.b),
but the tool is registered and the system prompt nudges the model
to route replies through it.

- `crates/config/src/types/brief.rs` — `BriefConfig { enabled,
  status_required, max_attachments }` with `#[serde(default)]`,
  `validate()`, `from`-style helper `is_active_with_assistant_mode`.
  6 schema tests verde (default / cap rejection / assistant-mode
  short-circuit / explicit-enable / YAML round-trip).
- `AgentConfig.brief: Option<BriefConfig>` field with
  `#[serde(default)]`. 49+ workspace fixture sites swept.
- `crates/core/src/agent/send_user_message_tool.rs` —
  `SendUserMessageTool` (`name = "send_user_message"`) with 4
  validation gates (message non-empty + size-cap, status enum,
  attachment-count cap, attachment path/file/canon validation).
  Output carries a `__nexo_send_user_message__` sentinel for
  downstream renderers + `BriefStatus { Normal, Proactive }`
  enum. `register_send_user_message_tool` is the boot helper.
  Tool def adapts schema `required` to `cfg.status_required`.
  16 tests verde (12 tool + 4 section gate).
- System-prompt section: `BRIEF_SECTION` constant +
  `brief_system_section(cfg, assistant_addendum_appended)` pure
  helper. Wired into `llm_behavior.rs` immediately after the
  Phase 80.15 assistant-mode addendum site, with the same
  cache-friendly stable ordering. Section is *skipped* when the
  assistant-mode addendum is already appended (avoid duplicating
  the directive — assistant mode hard-codes the same instruction).

**Robusto**: 4 ordered validation gates with typed errors; path
canonicalize against the agent workspace; reject directories;
hard 8 MiB body cap above the operator's `max_attachments` knob;
absolute attachment paths handled distinctly from relative.
**Óptimo**: stable system section (cache-warm), zero per-turn cost
when brief disabled (`Option<BriefConfig>` short-circuits at the
section gate AND the boot helper).
**Transversal**: provider-agnostic — uses our `Tool` trait + JSON
schema; no LlmClient assumptions.

**Deferred follow-ups**:
- **80.8.b** — channel-adapter hide-free-text filter. When brief
  is on AND the adapter opts into `hide_free_text`, only
  `send_user_message` calls render to the channel.
- **80.8.c** — `/brief` CLI slash command for live toggling
  through the setup wizard hook.
- **80.8.d** — main.rs boot wiring of
  `register_send_user_message_tool` per binding (waits on the
  pre-existing dirty-state resolution upstream of this work).



Phase 79.9 was opened as a "terse-mode toggle" but the leak's
BriefTool is a *gating mechanism* on a `SendUserMessage`
tool: when brief on, the agent's free-text output is hidden
from the operator and only `SendUserMessage` calls render in
the channel. Lets a long-running goal silence chatter and
emit just checkpoints.

**Reference (PRIMARY)**: `claude-code-leak/src/tools/BriefTool/BriefTool.ts:1-204`
+ `claude-code-leak/src/commands/brief.ts:1-130`.
**Reference (secondary)**: extend `crates/core/src/agent/`
with `brief_tool.rs` + `brief_state.rs`. Pairing channel
adapters (Phase 26 WA/TG) consume `brief_only` flag.

**Tool API** (Rust port):
```rust
SendUserMessage {
    message: String,
    attachments: Option<Vec<String>>,
    status: BriefStatus,  // Normal | Proactive
}
```
Returns `{ message, attachments?, sent_at: <ISO 8601> }`.

**Activation** (`is_brief_enabled`):
- `entitled` (build-time + binding-level `brief: enabled`)
- AND `(kairos_active OR user_msg_opt_in)`
- `user_msg_opt_in` set by: `--brief` CLI flag, `/brief`
  slash command, `agents.<id>.brief.default_on: true`,
  KAIROS daemon mode (`kairos_active == true`)
- 5-minute live re-check interval (refresh `brief_config`
  from binding policy via Phase 18 hot-reload)

**/brief slash command**:
- Toggle `brief_only` state on the active goal
- Inject system reminder on next turn:
  - On: "Use SendUserMessage tool for all user output —
    plain text is hidden"
  - Off: "SendUserMessage unavailable — reply with plain
    text"

**Channel adapter integration**:
- WA + TG `PairingChannelAdapter` checks `brief_only`:
  drops free-text messages, forwards `SendUserMessage`
  payloads
- Companion-tui (when shipped) renders a `[brief]` banner

**Done criteria**:
- New `crates/core/src/agent/brief_tool.rs` registers tool
  with `is_enabled()` gating
- `/brief` slash in companion-tui + `nexo agent brief
  on|off|status <goal_id>` CLI
- Phase 26 channel adapters honour `brief_only`
- 6 unit tests: entitlement matrix, opt-in toggle, attachment
  resolution, kairos-active forces brief, channel adapter
  drop-vs-forward, refresh interval

#### 80.9 — MCP channel routing + 5-step gate   ✅ MVP

MCP channel servers become inbound surfaces — Slack bots,
Telegram chats, iMessage relays. The runtime treats them as
trusted user inputs, gated by a 5-step filter and routed via a
NATS subject other processes can subscribe to. Outbound is
already covered by Phase 12 MCP client.

**Schema** (`crates/config/src/types/channels.rs`, ~250 LOC + 10
tests verde): `ChannelsConfig { enabled, approved, max_content_chars }`
+ `ApprovedChannel { server, plugin_source }` + per-binding
`InboundBinding.allowed_channel_servers`.

**Gate** (`crates/mcp/src/channel.rs`, ~700 LOC + 39 tests verde):
pure-fn `gate_channel_server` runs 5 ordered gates (capability,
killswitch, session, marketplace, allowlist) with typed `SkipKind`
reasons; `has_channel_capability` parses `experimental['nexo/channel']`
truthiness; `wrap_channel_message` produces the `<channel
source="...">...</channel>` XML with attribute-key whitelist +
control-char escape. Inner content stays verbatim.

**Inbound parsing**: `parse_channel_notification` →
`ChannelInbound { server_name, content, meta, session_key }`.
`ChannelParseError` thiserror-typed (UnexpectedMethod,
MissingParams, MissingContent, InvalidMeta, EmptyServerName).
Content cap + meta-key whitelist applied.

**Session correlation**: `ChannelSessionKey::derive` picks the
first known threading key (`thread_ts`, `chat_id`,
`conversation_id`, `room_id`, `channel_id`, `thread_id`, `to`) so
multi-thread channels route to consistent agent sessions.
Deterministic across processes.

**Cross-process routing**: `channel_inbox_subject(binding, server)`
→ `mcp.channel.<binding>.<server>` (dot-stripped); wildcard
`mcp.channel.>`. `ChannelDispatcher` async trait with
`DispatchError`. `ChannelEnvelope { schema=1, binding_id, server_name,
content, meta, session_key, rendered (XML), sent_at_ms,
envelope_id }` — pre-renders the `<channel>` wrap so subscribers
don't depend on this crate.

**Per-process registry**: `ChannelRegistry`
(`RwLock<BTreeMap<(binding,server), RegisteredChannel>>`) tracks
active registrations; `SharedChannelRegistry = Arc<...>` typedef.

**LLM-side introspection**
(`crates/core/src/agent/channel_list_tool.rs`, ~140 LOC + 3 tests
verde): `channel_list` tool returns
`{ binding_id, count, servers: [ChannelSummary] }`. Read-only,
auto-approve-friendly. `register_channel_list_tool` boot helper.

**Defensive properties**: 5 ordered gates, each Skip with typed
reason; XML attribute injection blocked at three layers
(meta-key whitelist, value escape with control-chars + line
breaks as numeric refs, source-attr escape); plugin-source
mismatch surfaces a *distinct* `Marketplace` skip; content cap
defends conversation context.

**Counts**: 39 channel + 10 schema + 3 tool = **52 new tests
verde**. Workspace: 397 nexo-mcp + 763 nexo-core + 193 nexo-config.

**Deferred follow-ups**:

- **80.9.b** ✅ MVP — permission relay protocol surface.
  `crates/mcp/src/channel_permission.rs` (~620 LOC + 27 tests
  verde) ships every wire frame + every primitive the
  approval flow needs:
  - `PERMISSION_REQUEST_METHOD`
    (`notifications/nexo/channel/permission_request`) +
    `PERMISSION_RESPONSE_METHOD`
    (`notifications/nexo/channel/permission`) +
    `PERMISSION_REQUEST_SCHEMA_VERSION = 1`.
  - `PermissionBehavior { Allow, Deny }` enum with
    `as_str()`, `parse(raw)` (case-insensitive),
    `Serialize/Deserialize` round-trip.
  - `PermissionRequestParams { schema, request_id, tool_name,
    description, input_preview }` (outbound payload) +
    `PermissionResponseParams { request_id, behavior }`
    (inbound) + `PermissionResponse { request_id, behavior,
    from_server }` (audit-shaped bundle the registrant
    receives).
  - `short_request_id(tool_use_id)` — FNV-1a hash + base-25
    5-letter encode (alphabet a-z minus `l` to avoid the 1/I
    confusable). Substring blocklist with re-hash on hit so
    generated IDs never spell the obvious offensive
    5-letter words. Verified against 2000 sampled inputs
    in tests.
  - `truncate_input_preview(value)` — JSON-serialise +
    truncate to 200 chars with `…` suffix. `(unserializable)`
    on serde failure.
  - `parse_permission_reply(text)` — server-side helper
    parsing `^\s*(y|yes|n|no)\s+([a-km-z]{5})\s*$/i`.
    Lowercase prefix tolerated (phone autocorrect's
    capitalisation), ID itself must be lowercase to keep
    the alphabet's anti-confusable promise.
  - `PendingPermissionMap` (`Mutex<HashMap<String,
    oneshot::Sender>>`) — process-local rendezvous. `register`
    returns the receiver; `resolve(response)` matches and
    drops the sender (returns `false` when the receiver was
    already dropped — that's the "lost the race to the
    local prompt" case, not an error). `cancel(id)` releases
    a pending entry without resolving. `len()` for telemetry.
  - `parse_permission_response(method, params)` —
    `ChannelParseError`-shaped error type
    (`UnexpectedMethod` / `MissingParams` /
    `MissingRequestId` / `InvalidBehavior`).
  - `PermissionRelayDispatcher` async trait + thin
    `McpPermissionRelayDispatcher<C>` impl. The seam where
    the runtime asks a server to surface a prompt; the trait
    keeps the approval flow testable against stubs.
  - `ClientEvent::ChannelPermissionResponse { params }`
    variant + `channel_permission_response_event(params)`
    constructor. `client.rs` detects
    `PERMISSION_RESPONSE_METHOD` and emits the typed event
    with captured params (mirror of 80.9.c channel-message
    detection).
  - `events.rs` total: 4 new variants + 4 new tests verde.
  - **Counts**: 450 nexo-mcp tests verde (was 421, +29 across
    27 permission + 2 events).
  - **Deferred 80.9.b.b** — the higher-level approval-flow
    integration that races the channel reply against the
    local prompt + writes the audit row. Lands in
    `nexo-driver-permission` once it grows a pluggable
    seam for "another approver might claim this." The
    protocol surface above is complete and stable.
- **80.9.c** ✅ MVP — live notification dispatch wiring at the
  MCP client layer. `McpCapabilities.experimental: Value`
  retains the raw block from `initialize` so
  `has_channel_capability` can inspect it. New
  `ClientEvent::ChannelMessage { params }` variant +
  `channel_message_event(params)` constructor; `client.rs`
  detects `CHANNEL_NOTIFICATION_METHOD` and emits the typed
  event with captured params. `BrokerChannelDispatcher`
  serialises `ChannelEnvelope` and publishes via `AnyBroker`
  on `mcp.channel.<binding>.<server>`.
  `ChannelInboundLoop` + `ChannelInboundLoopConfig` +
  `ChannelInboundLoopHandle` drive the stream:
  gate-once-on-spawn → register → consume `ChannelMessage`
  events → `parse_channel_notification` →
  `dispatcher.dispatch`. Survives parse errors + dispatch
  failures + slow-consumer Lag; cleans up the registry on
  cancel and on events-closed. 8 new tests verde (5 loop +
  1 broker dispatcher + 2 events).
- **80.9.d** ✅ MVP — agent-side bridge from
  `mcp.channel.>` into the runtime as user inbound. New
  `crates/mcp/src/channel_bridge.rs` (~390 LOC + 9 tests
  verde) ships:
  - `SessionRegistry` async trait + `InMemorySessionRegistry`
    (`RwLock<BTreeMap<ChannelSessionKey, SessionEntry>>`)
    with `resolve` (first-seen creates uuid, repeats refresh
    timestamp), `gc_idle(max_idle_ms)` (`0` is no-op
    sentinel), `len`, `snapshot`. Persistent SQLite-backed
    impl deferred 80.9.d.b.
  - `ChannelInboundEvent { binding_id, server_name,
    session_id, session_key, content, meta, rendered,
    envelope_id, sent_at_ms }` — typed payload the bridge
    hands to the sink (no JSON re-parsing).
  - `ChannelInboundSink` async trait + `SinkError` (Rejected
    / Other). The caller decides which intake path the
    message follows — typically synthesising an inbound on
    `agent.intake.<binding_id>` so the existing pairing /
    dispatch / rate-limit gates apply unchanged.
  - `ChannelBridge` + `ChannelBridgeConfig` (broker, registry,
    sink, subject defaults to `mcp.channel.>`,
    `gc_interval_ms` default 5 min, `max_idle_ms` default
    1 h). `spawn(cancel)` returns a `ChannelBridgeHandle`
    with two join-handles: the consumer that drains the
    broker subscription + an optional GC ticker. Both stop
    cleanly on cancel.
  - Threading: same `session_key` → same `session_id` across
    messages and across processes; distinct keys (Slack
    threads, Telegram chats) split into distinct sessions.
  - Defensive: malformed envelopes are warn-logged + dropped
    (not fatal); sink errors warn-log + continue (loop
    survives); GC `0` skips eviction; subject filter narrows
    subscription for tenancy isolation.
  - 9 tests verde: registry first-seen / distinct keys / GC
    eviction / GC zero noop; bridge resolves+delivers, logs
    sink failures, threads distinct keys, narrows subject
    filter, GC task runs.
- **80.9.d.b** ✅ MVP — persistent SQLite `SessionRegistry`.
  `crates/mcp/src/channel_session_store.rs` (~250 LOC + 9
  tests verde) implements the same `SessionRegistry` trait
  against SQLite so `session_key → session_uuid` survives
  daemon restarts (Slack threads, Telegram chats, iMessage
  conversations don't have to re-introduce themselves on
  every reboot). Schema:
  ```sql
  CREATE TABLE IF NOT EXISTS mcp_channel_sessions (
      key          TEXT PRIMARY KEY,
      session_id   TEXT NOT NULL,
      last_seen_ms INTEGER NOT NULL
  );
  CREATE INDEX IF NOT EXISTS idx_channel_sessions_last_seen
      ON mcp_channel_sessions(last_seen_ms);
  ```
  Idempotent migration (`CREATE … IF NOT EXISTS`); WAL +
  `synchronous=NORMAL` via the same `SqliteConnectOptions`
  shape Phase 71/72 stores use. `resolve` is a single
  UPSERT with `RETURNING` (SQLite ≥ 3.35) so first-seen and
  refresh take one round-trip. `gc_idle(max_idle_ms)` is a
  bulk DELETE; `0` and negative values are no-op sentinels.
  `len()` for telemetry. `snapshot()` test helper for
  assertions. Fail-safe semantics — UPSERT errors warn-log
  and return an ephemeral uuid for the in-flight turn so
  threading-this-turn is preserved even when persistence
  is degraded.
  9 tests verde: first-seen / distinct-keys / refresh /
  GC eviction / GC zero-noop / GC negative-noop /
  snapshot / **schema idempotent across reopens** (real
  tempfile, not `:memory:`) / **concurrent-safe UPSERT**
  (two parallel resolvers on the same key agree on one uuid).

**80.9 outbound + boot helpers ✅ MVP**:
`ApprovedChannel.outbound_tool_name: Option<String>` (default
`Some("send_message")`) + `resolved_outbound_tool_name()`
helper; `RegisteredChannel` snapshots the resolved value at
register-time so a config reload mid-session doesn't change
which tool an in-flight reply reaches.
`crates/core/src/agent/channel_send_tool.rs` (~200 LOC + 4
tests verde): `channel_send` routes
`(server, content?, arguments?)` through
`SessionMcpRuntime.call_tool` with 5 ordered gates (server
present, registered for binding, arguments-shape, 64 KiB
content cap, MCP runtime wired); content shortcut populates
the argument's `text` key when the operator hasn't supplied
an explicit `arguments` object.
`crates/core/src/agent/channel_status_tool.rs` (~170 LOC +
4 tests verde): `channel_status` diagnoses one server or
every registered server; renders connection state + plugin
source + resolved outbound name + permission-relay flag +
registered-at timestamp.
`crates/mcp/src/channel_boot.rs` (~200 LOC + 5 tests verde):
`ChannelBootContext { broker, registry, session_registry,
dispatcher }` ties the four shipped pieces into one value
that main.rs constructs once.
`ChannelBootContext::in_memory(broker)` is the default
factory; `bridge_config(sink)` + `spawn_bridge(sink, cancel)`
cover the per-process spawn site.
`build_inbound_loop_config(...)` + `enumerate_targets(cfg,
binding_allowlist)` cover the per-(binding, server) spawn.
13 nexo-config channels tests verde (was 10, +3 around
outbound-tool resolution + override validation). **Workspace
counts**: 421 nexo-mcp, 771 nexo-core, 13 nexo-config
channels.

**main.rs hookup ✅ MVP** — closed the seam from "MCP
server emits notification" to "agent receives `<channel>`
user message" end-to-end. `src/main.rs` now wires:

- `ChannelBootContext::in_memory(broker)` once at boot
  right after the broker is ready. Holds the shared
  `ChannelRegistry` + `InMemorySessionRegistry` +
  `BrokerChannelDispatcher`.
- `IntakeChannelSink` (concrete impl of
  `nexo_mcp::channel_bridge::ChannelInboundSink`):
  serialises each `ChannelInboundEvent` into a JSON
  envelope and publishes on `agent.channel.inbound` so
  the existing intake task picks it up under the same
  pairing / dispatch / rate-limit gates as every other
  channel inbound. Provider-agnostic.
- `channel_boot.spawn_bridge(sink, channel_shutdown)` —
  one consumer task + one GC ticker per process. Both
  stop cleanly on the shared cancellation token. Bridge
  spawn failures warn-log but never block the daemon.
- Per-agent: when `agent_cfg.channels` is `Some(cfg)` AND
  any binding has a non-empty
  `allowed_channel_servers`, register `channel_list` +
  `channel_send` + `channel_status` tools agent-scoped
  (per-binding granularity is the 80.9.j follow-up).
- Per-agent post-MCP: walk `rt.clients()` and spawn a
  `ChannelInboundLoop` per `(server, binding=agent_id)`
  when the union of binding allowlists contains the
  server. The loop reads
  `client.capabilities().experimental` to detect the
  `nexo/channel` capability flags, runs the one-shot
  gate, and either registers + consumes
  `ChannelMessage` events or surfaces a typed
  `Skipped { kind, reason }` log.
- Smoke tested locally: `nexo channel list --json` and
  `nexo channel doctor` work cleanly against the
  current channels-off YAML; no panics, no missing
  fields, daemon boot path compiles workspace-wide.

- **80.9.e** ✅ MVP — operator CLI: `nexo channel list/doctor/test`.
  Three new `Mode` variants (`ChannelList`, `ChannelDoctor`,
  `ChannelTest`) + parser arms + dispatch + run fns
  (`run_channel_list`, `run_channel_doctor`, `run_channel_test`).
  All read-only against the YAML — no daemon required.
  - `channel list [--config=<path>] [--json]` walks every agent
    and surfaces `(enabled, approved_servers[], bindings[])` per
    agent. `bindings` is filtered to those with a non-empty
    `allowed_channel_servers` so cluttered YAML stays readable.
  - `channel doctor [--config=<path>] [--binding=<id>] [--json]`
    runs the static half of the gate against every
    `(agent, binding, server)` triple. Capability is *assumed*
    declared (the doctor cannot probe a live MCP server),
    `plugin_source` is read from the approved entry, and
    gates 2/3/5 run normally. Each row reports `WOULD REGISTER`
    or a typed `SKIP { kind, reason }`. The doctor also
    cross-checks `approved` entries that no binding lists and
    surfaces them as `NOT BOUND` so an operator who configured
    a server but forgot to bind it sees the gap immediately.
  - `channel test <server> [--binding=<id>] [--content=...]
    [--json]` synthesises a notification with sample
    `chat_id`/`user` meta, runs `parse_channel_notification` +
    `wrap_channel_message`, and prints the model-facing
    `<channel source="...">...</channel>` block plus the derived
    `session_key`. Cheap dry-run for tuning meta-key
    whitelists or verifying content-cap behaviour.
  - `load_app_config_for_channels(config_override, config_dir)`
    helper — accepts a directory or single-file `--config`
    override (walks up to parent for files), defaults to the
    global `--config-dir`.
  - Smoke tested locally; YAML without channels output renders
    as `(no channel-using bindings found)` and `channel list`
    surfaces `enabled: false` per agent without panicking.
- **80.9.f** ✅ MVP — hot-reload re-evaluation.
  `ChannelRegistry::reevaluate(&ReevaluateInputs)` walks
  every active registration and re-runs the static half of
  the gate against the operator's *current* config. Entries
  that no longer pass — killswitch off, server removed from
  `approved`, plugin_source pinned to a new value, binding
  deleted — get unregistered. Returns a typed
  `ReevaluateReport { kept, evicted: Vec<{binding,
  server, kind, reason}> }` so observability + setup-doctor
  can surface what changed.
  - `ReevaluateInputs { by_binding: HashMap<String,
    ReevaluateBinding>}` + `ReevaluateBinding { cfg,
    allowed_servers }` — caller produces this from the
    freshly-loaded YAML inside the Phase 18 post-reload
    hook.
  - `channel_boot::build_reevaluate_inputs(iter)` builder
    helper accepting `(binding_id, Arc<ChannelsConfig>,
    Vec<String>)` tuples.
  - 6 tests verde: keeps-passing / evicts-on-killswitch-off /
    evicts-when-removed-from-approved /
    evicts-when-binding-disappears / evicts-on-plugin-source-mismatch /
    partial-some-kept-some-evicted.
  - 465 nexo-mcp tests verde (was 459, +6).
- **80.9.g** ✅ MVP — per-channel rate limits.
  `ChannelRateLimit { rps, burst }` schema with
  `is_active()` + `validate(label)` (rejects negative rps,
  NaN, soft cap of 1000 rps to catch typos like
  `rps_per_minute`). `ChannelsConfig` gains
  `default_rate_limit: Option<ChannelRateLimit>` and
  `ApprovedChannel.rate_limit: Option<ChannelRateLimit>` —
  per-server override wins, falls back to default, `None`
  when both inactive (no throttling). `resolve_rate_limit`
  helper picks the effective config; `validate()` propagates
  the per-entry validation.

  Token bucket implementation in
  `crates/mcp/src/channel.rs`: `TokenBucket::new(rl)` +
  `try_acquire()` with lazy refill on each call (no
  background task; bucket state is `Mutex<{tokens,
  last_refill}>`). `ChannelInboundLoop` builds an
  `Option<Arc<TokenBucket>>` once at spawn from
  `cfg.resolve_rate_limit(server_name)` and consults it
  before each dispatch — empty bucket drops the message
  with a structured warn (`"channel inbound dropped — rate
  limit exceeded"`). 5 new tests verde (burst-then-blocks /
  refills-at-rps / refill-caps-at-burst / drops-when-bucket-empty /
  unrate-limited-dispatches-all). 8 schema tests verde
  (active / negative-rps / nan / excessive / per-server-override /
  default-fallback / inactive-returns-none / yaml-round-trip).
  Workspace counts: 21 nexo-config channels, 109 nexo-mcp
  channel + bridge + bucket = 130 channel tests verde across
  schema + runtime.

- **80.9.j** ✅ MVP — per-binding tool granularity.
  Channel tools (`channel_list` / `channel_send` /
  `channel_status`) gain a `new_dynamic(registry)`
  constructor that resolves `binding_id` from
  `ctx.effective` at call time, falling back to
  `ctx.agent_id` when no binding context exists (heartbeat,
  delegate receive, tests). `resolve_binding_id(ctx)` lives
  in `channel_list_tool` and is shared by the other tools.
  `main.rs` now uses the dynamic constructors AND spawns
  one `ChannelInboundLoop` per `(binding, server)` triple
  with `binding_id = "<plugin>:<instance>"` — the registry
  view each tool sees scopes to the active binding instead
  of the agent. 2 new tests verde (static-binding-stored /
  dynamic-binding-resolution-deferred).
- **80.9.h** ✅ MVP — Phase 72 turn-log marker
  `source: "channel:<server>"` for audit. `TurnRecord.source:
  Option<String>` field added with `#[serde(default)]` so older
  callers / persisted JSON parse cleanly. SQLite migration is
  the same idempotent `ALTER TABLE … ADD COLUMN` shape the
  cron `permanent` column uses; "duplicate column" errors are
  tolerated. New `idx_goal_turns_source` index keeps
  per-server filtering cheap. INSERT + UPSERT on
  `(goal_id, turn_index)` carry the column; both `tail` and
  `tail_since` SELECTs include it. Pure-fn helpers
  `format_channel_source(server)` /
  `parse_channel_source(source)` keep the `channel:` prefix
  stable across the codebase. 4 new tests verde
  (round-trip / default-none-for-legacy / replay-idempotent /
  prefix render+parse). Total nexo-agent-registry: 51 lib
  tests verde.

  The channel inbound sink populates the field at intake
  time; every other intake path (paired user inbound, cron
  fire, agent-to-agent delegate, heartbeat, poller) leaves
  it `None` — `EventForwarder` documents this explicitly.
- **80.9.i** — `channel_send` LLM tool (stable wrapper around
  the server's outbound `send_message`).



A KAIROS "channel" is an MCP server (Discord, Slack, SMS,
etc.) that declares `capabilities.experimental['claude/channel']`.
The server emits `notifications/claude/channel` for inbound
user messages and we invoke its tool to send outbound. The
inbound payload is wrapped in `<channel source="server-name">`
XML when injected into the model context. Outbound permission
flow is structured (no "yes CODE" string parsing) via
`ChannelPermissionRequestParams`.

**Reference (PRIMARY)**: `claude-code-leak/src/services/mcp/channelNotification.ts:1-316`.
**Reference (secondary)**: extend `crates/mcp/` with channel
capability + `crates/core/src/agent/` consumes the wrapper +
Phase 26 pairing routes inbound through pairing gate.

**7-step gate** (port verbatim — `channelNotification.ts:191-316`):
1. **Capability**: server advertises `experimental.claude/channel`
2. **Runtime gate**: per-binding `channels.enabled` (replaces
   `tengu_harbor`)
3. **Auth**: OAuth-only path (matches our subscription auth
   gate from Phase 15.x)
4. **Org policy**: `agents.<id>.channels.allowed_servers`
   allowlist when binding has org-managed flag
5. **Session allowlist**: CLI `--channels plugin:slack@anthropic
   server:foo` parses into `RuntimeSnapshot::allowed_channels`
6. **Plugin marketplace verification**: declared `marketplace`
   matches installed source (matches Phase 31 plugin marketplace
   work — gate via that or DEFER)
7. **Allowlist final check**: `allowed_channel_plugins` setting
   OR developer override (`NEXO_DANGEROUSLY_LOAD_DEV_CHANNELS=true`)

**XML wrapping**:
```xml
<channel source="slack-prod" thread_id="C123" user="U456">
  Body of the inbound message
</channel>
```
Meta keys validated: `^[a-zA-Z_][a-zA-Z0-9_]*$` (anti
attribute-injection). Unknown keys dropped, not errored.

**Done criteria**:
- `crates/mcp/src/channel.rs` — capability discovery + 7-step
  gate
- `crates/core/src/agent/context.rs` — inbound wrapping
- `nexo --channels` CLI flag parses into binding override
- 8 unit tests covering each gate step deny-path
- 1 integration test: mock MCP server emits notification,
  agent sees it wrapped, sends back via `SendUserMessage` →
  server tool invoked

#### 80.10 — Agent SessionKind + BG sessions   ✅ MVP

**Shipped**: nuevo `SessionKind` enum en
`crates/agent-registry/src/types.rs` con 4 variants
(`Interactive` default / `Bg` / `Daemon` / `DaemonWorker`) +
helpers `as_db_str` / `from_db_str` / `survives_restart` (true
para Bg/Daemon/DaemonWorker, false Interactive). `AgentHandle`
gana field `kind: SessionKind` con `#[serde(default)]` para
backward-compat — rows persisted antes de 80.10 deserialise como
`Interactive` automáticamente. Schema migration v5 idempotent
via `add_column_if_missing("kind TEXT NOT NULL DEFAULT 'interactive'")`
+ index `idx_agent_registry_kind` para acelerar `list_by_kind`.
UPSERT extendido con bind 11 (`kind = excluded.kind`); `row_to_handle`
lee la columna como column-source-of-truth (column wins over
JSON blob). Helpers nuevos en `SqliteAgentRegistryStore`:
`list_by_kind(SessionKind)` para `nexo agent ps --kind=bg` y
`reattach_running_kind_aware()` que flippa Running → LostOnRestart
SOLO para `kind = 'interactive'` (Bg / Daemon / DaemonWorker
keep Running). Workspace fixture sweep aplicó perl multi-line
replace `s/^(\s*)plan_mode: None,$/$1plan_mode: None,\n$1kind:
nexo_agent_registry::SessionKind::Interactive,/` a 14+ struct
literals across `crates/{agent-registry,core,dispatch-tools}` +
test files. CLI surface en `src/main.rs`: `Mode::AgentRun {
prompt, bg, db, json }` + `Mode::AgentPs { kind, all, db, json }`
+ 2 parser arms + 2 dispatch arms + helper
`resolve_agent_db_path` (3-tier mirror de Phase 80.1.d:
`--db` > `NEXO_STATE_ROOT` env > XDG default
`~/.local/share/nexo/state/agent_handles.db`).
**`run_agent_run`** valida prompt non-empty, abre store, INSERT
nuevo `AgentHandle` con `goal_id = Uuid::new_v4()`,
`status = Running`, `phase_id = "cli-bg"|"cli-run"`,
`kind = if bg { Bg } else { Interactive }`, prints goal_id +
detach hint. **`run_agent_ps`** abre store, dispatcha
`list_by_kind(parsed)` o `list()`, filtra Running unless `--all`,
renders markdown table o JSON. 13 nuevos tests verde:
nexo-agent-registry 8 (default-is-interactive / db round-trip
all variants / from-db-str rejects unknown / survives-restart
matrix / agent-handle serde-default-kind via field-strip /
store-insert-with-kind round-trip / list-by-kind filters /
reattach-kind-aware keeps Bg) + nexo-rs (bin) 5 (resolve-db
override-wins / resolve-db env-fallback / run-rejects-empty /
run-bg-inserts-bg / run-no-bg-inserts-interactive / ps-empty-db-
friendly / ps-filters-by-kind / ps-rejects-invalid-kind). CLI
smoke verified manually:
`mkdir -p /tmp/nexo80-10/state && NEXO_STATE_ROOT=/tmp/nexo80-10/state
./target/debug/nexo agent ps` → friendly message;
`nexo agent run --bg "test goal here"` → goal_id printed;
`nexo agent ps` → 1 row (Running/bg);
`nexo agent ps --kind=interactive` → "(no rows match)".

**Three-pillar audit**: robusto (13+ tests; migration `IF NOT
EXISTS` idempotent; default Interactive keeps fixtures + Phase
71 backward-compat verde; reattach kind-aware preserva
semántica; ps query handles missing DB gracefully), óptimo
(single new column, no separate table; reuses existing list +
upsert paths; ps RO pool), transversal (provider-agnostic
SQLite + CLI; cero LLM-provider touchpoints; works under any
LlmClient impl).

**Deferred follow-ups**:
- **80.10.b** — `nexo agent attach <goal_id>` TTY re-attach
  (= Phase 80.16 ya separado).
- **80.10.c** — Daemon supervisor process for `Daemon` /
  `DaemonWorker` kinds (separate process lifecycle).
- **80.10.d** — `nexo agent kill <goal_id>` graceful abort
  signal.
- **80.10.e** — `nexo agent logs <goal_id>` re-stream.
- **80.10.f** — Phase 77.17 schema-migration system integration
  (versioned `user_version` bump for `kind` column).
- **80.10.g** — Daemon-side pickup of queued goals: today the
  CLI inserts the row but no daemon worker consumes it
  automatically. Operator must run `nexo agent attach` (after
  80.16) or invoke a future supervisor; for now the row sits
  Running until manually transitioned. Document.

**Original spec for follow-ups** (preserved):

Today `agent_registry::AgentRunStatus` tracks lifecycle but
not *kind*. KAIROS distinguishes `interactive | bg | daemon |
daemon-worker` so `--continue` can skip live BG sessions
(Phase 71 reattach has the right machinery but no kind
discriminator). New `nexo agent run --bg` detached spawn
mode + `nexo agent ps` listing.

**Reference (PRIMARY)**: `claude-code-leak/src/utils/concurrentSessions.ts:1-204`
(`SessionKind`, PID file, `registerSession`,
`isBgSession`).
**Reference (secondary)**: extend
`crates/agent-registry/src/types.rs` with `SessionKind` enum;
`crates/dispatch-tools/` adds `nexo agent ps` subcommand;
`SqliteAgentRegistryStore` schema migration adds `kind`
column.

**Done criteria**:
- `SessionKind` enum (`Interactive | Bg | Daemon | DaemonWorker`)
  on `AgentHandle`
- `nexo agent run --bg <prompt>` spawns detached goal,
  prints goal-id, returns immediately
- `nexo agent ps [--all] [--kind=bg]` lists running goals
  with kind, channel, prompt-summary, age
- `nexo agent attach <goal_id>` re-attaches a BG goal to
  current TTY (uses Phase 67 NATS subjects for live output)
- Phase 71 reattach honours `kind` — `Daemon` rows survive
  reattach as `LostOnRestart`, `Bg` rows do too but emit
  the catch-up `notify_origin` flagged with "background"
- Schema migration v3 adds `kind`, `name`, `log_path` columns
  (Phase 77.17 migration system)
- 5 unit tests + 1 integration: spawn 3 BG goals, ps lists
  all 3, attach to one, exit detaches without killing

#### 80.11 — Agent inbox subject + ListPeers + SendToPeer   ✅ MVP

**Shipped (publisher-only)**: nuevo `crates/core/src/agent/inbox.rs`
con `inbox_subject(GoalId) -> String` helper que rinde
`agent.inbox.<goal_id>` + struct `InboxMessage { from_agent_id,
from_goal_id, to_agent_id, body, sent_at, correlation_id: Option<Uuid> }`
+ constants `INBOX_SUBJECT_PREFIX`, `MIN_BODY_CHARS=1`,
`MAX_BODY_BYTES=64*1024`. 3 unit tests verde (subject format,
serde round-trip, correlation_id-omitted-when-none).

`crates/core/src/agent/list_peers_tool.rs` ships read-only
`ListPeersTool` LLM tool (~80 LOC + 1 test) — returns JSON
`{ peers: [{ agent_id, description, reachable }] }` excluding self,
con reachability calculada via `EffectiveBindingPolicy.allowed_delegates`
glob-matching pattern reusing peer_directory pattern. No-arg shape
(`additionalProperties: false`).

`crates/core/src/agent/send_to_peer_tool.rs` (~280 LOC + 11 tests
verde) ships `SendToPeerTool { lookup: PeerGoalLookup }` con
`PeerGoalLookup = Arc<dyn Fn(&str) -> Vec<GoalId> + ...>` injected
closure que el operator wires a `nexo_agent_registry::AgentRegistry::list`
filtered by Running goals — keeps tool free of agent-registry
dep. Handler valida (1) `to` non-empty, (2) `to != self_agent_id`,
(3) `message` non-empty, (4) body ≤ 64 KB, (5) verifica peer
exists in `PeerDirectory` (fast-path "unknown agent_id"
unreachable when not present), (6) lookup retorna live goal_ids
(unreachable "no live goals" si vacío). Para cada live goal,
publica `Event::new(inbox_subject(goal), source=agent_id, payload=msg)`
via `ctx.broker.publish`; recolecta `delivered_to: Vec<goal_id>`
+ `unreachable_reasons: Vec<String>`. JSON output:
`{ delivered_to: [...], unreachable_reasons: [...] }`. Sender
goal_id viene de `ctx.session_id.map(GoalId).unwrap_or_else(GoalId::new)`
para casos sin session activo (heartbeat path) — provenance
preservada via `from_agent_id` igual.

11 tests cubren `tool_def_shape`, `empty_to_errors`,
`missing_to_errors`, `missing_message_errors`, `empty_message_errors`,
`self_send_rejected`, `unknown_agent_id_returns_unreachable`,
`no_live_goals_returns_unreachable`,
`live_peer_publishes_and_returns_delivered`,
`oversize_message_rejected`, `correlation_id_round_trips` (este
último subscribe al inbox subject + verifica que el correlation_id
arrive intact por el wire). Plus 1 list_peers tool_def shape test
+ 3 inbox subject/payload tests = **15 nuevos tests verde** total.

`PeerDirectory.peers() -> &[PeerSummary]` accessor agregado
porque el field interno era private — slice accessor para tools
que necesitan la lista raw (vs el rendering markdown del system
prompt). Backward-compat preserved.

**Three-pillar audit**: robusto (15 tests; 6 validation gates en
SendToPeer; defensive arg parsing; broker publish failure
handling via unreachable_reasons; PeerDirectory existence check
fast-path; race-safe — peer goal terminating between calls falls
through unreachable not panic), óptimo (PeerDirectory cached at
boot; AgentRegistry lookup via injected closure es in-memory;
broker publish fire-and-forget zero blocking; for-loop O(N)
sobre live goals es trivial), transversal (cero LLM-provider
touchpoints; pure JSON tool surface; NATS subject contract
trabaja igual local broker o cluster remoto).

**Wiring point** (operator opts in cuando ready): caller-side
debe registrar las 2 tools en el ToolRegistry del binding's
agente con la closure `lookup` apuntando a `agent_registry::list`
filtered por `agent_id` + Running. Sin esa registration las
herramientas no aparecen en el LLM surface.

**Deferred follow-ups**:
- **80.11.b** — Receive side router + per-goal buffer + render
  helper ✅ **MVP** — shipped: nuevo
  `crates/core/src/agent/inbox_router.rs` (~280 LOC + 17 tests).
  `InboxRouter<B: BrokerHandle>` mirrors el patrón de Phase 79.6
  `TeamMessageRouter`: `Arc<Self>` con `dashmap::DashMap<GoalId,
  Arc<InboxBuffer>>` + `spawn(cancel: CancellationToken) ->
  JoinHandle` que subscribes a `agent.inbox.>` wildcard pattern
  una sola vez por proceso. `dispatch_inbound(ev)` parses
  `agent.inbox.<goal_id>` → buffer-on-demand semantics: si el
  goal no está registrado, crea fresh buffer y queues el
  mensaje (race-safe — peer puede send antes de que el consumer
  goal arranque, sin perder mensaje); idempotent
  `register(goal_id)` retorna existing buffer si ya existe (goal
  resume safe). `InboxBuffer { queue: Mutex<VecDeque<InboxMessage>> }`
  con `MAX_QUEUE = 64` cap + FIFO eviction (warn log cuando
  evicta — long-idle goals no acumulan unbounded backlog).
  `push(msg) -> bool` (true cuando evicted), `drain() -> Vec`
  atomic empty-and-return preserva chronological order, `len()`
  / `is_empty()` lectura para read tools como `agent_status`.
  `parse_goal_from_subject(subject) -> Option<GoalId>` defensive
  parser (rechaza unknown prefix + non-uuid suffix con debug
  log, no panic). Pure-fn `render_peer_messages_block(messages:
  &[InboxMessage]) -> Option<String>` returns None cuando empty
  para que callers usen `if let Some(block) = render(...)`
  inline en `channel_meta_parts.push(block)`; render shape:
  ```
  # PEER MESSAGES

  <peer-message from="researcher" sent_at="2026-04-30T..." correlation_id="...">
  body
  </peer-message>
  ```
  Correlation_id attribute renderizado SOLO cuando Some (skip
  attribute when None — minimal noise). Cancellation token
  shutdown clean: spawn task observa `cancel.cancelled()` en
  `tokio::select!` arm. Subscriber failures → `tracing::warn!`
  + early return (matches `team_message_router` pattern;
  inbox offline preferable to panic loop).

  17 nuevos tests verde:
  - `buffer_push_drain_round_trip` — basic FIFO semantics
  - `buffer_drain_empty_returns_empty_vec`
  - `buffer_evicts_oldest_at_cap` — push 64+1, verify eviction
  - `parse_goal_from_subject_valid` / `_rejects_unknown_prefix`
    / `_rejects_non_uuid_suffix` (3 tests defensive parser)
  - `render_empty_returns_none`
  - `render_single_message_includes_from_and_body` (no
    correlation_id attribute when None)
  - `render_with_correlation_id_includes_attribute`
  - `render_preserves_chronological_order` (3 messages → asserts
    body positions in output)
  - `router_register_idempotent_returns_same_buffer` (push via
    one Arc clone, drain via another → same buffer instance)
  - `router_dispatch_inbound_pushes_to_buffer` (synthetic event
    direct dispatch)
  - `router_buffer_on_demand_for_unregistered_goal` (send →
    register later → drain sees buffered msg)
  - `router_drops_malformed_subject` / `_drops_malformed_payload`
  - `router_forget_drops_buffer`
  - `router_spawn_subscribes_and_routes_end_to_end` (full
    pubsub via `AnyBroker::local()`: spawn router → publish →
    sleep → drain buffer → assert message present → cancel)

  **Three-pillar audit**: robusto (17 tests; bounded buffer cap
  64; subscriber cancellation clean; malformed subject /
  payload dropped with debug log; buffer-on-demand handles
  fast-spawn race; concurrent register/dispatch safe via
  dashmap; subscriber-failure log-and-exit pattern), óptimo
  (single broker subscriber per process; dashmap O(1) lookup;
  per-buffer Mutex held only for microsecond push/drain
  windows; drain is single allocation), transversal (pure
  NATS subject + JSON payload + in-memory VecDeque; cero
  LLM-provider touchpoints; transversal Anthropic / MiniMax /
  OpenAI / Gemini / DeepSeek / xAI / Mistral).

  **Wiring point** (operator opts in cuando ready):
  1. Boot: `let router = InboxRouter::new(broker.clone()); let
     handle = router.spawn(cancel.clone());` (single per-process
     spawn).
  2. Per-goal startup: `let buf = router.register(goal_id);` —
     stash on the goal's runtime context.
  3. Per-turn loop: drain `buf` at turn start, render via
     `render_peer_messages_block(&drained)`, push the
     `Some(block)` to `channel_meta_parts` adjacent to existing
     proactive / coordinator hints (Phase 80.15 pattern).
  4. Goal terminal: `router.forget(goal_id);` releases the buffer.

  **Deferred follow-ups**:
  - **80.11.b.b** — Hook into `llm_behavior.rs` per-turn loop
    drain + render injection (1-line snippet adjacent to
    Phase 80.15 assistant addendum); blocked on dirty-state
    pattern same as 80.15.g / 80.17.b.b / etc.
  - **80.11.b.c** — main.rs router spawn + per-goal
    register/forget on goal lifecycle hooks.
- **80.11.c** — Broadcast `to: "*"` with cap (linear in team
  size, expensive) + warning shape per upstream pattern.
- **80.11.c** — Broadcast `to: "*"` with cap (linear in team
  size, expensive) + warning shape per upstream pattern.
- **80.11.d** — Cross-machine inbox via NATS cluster (works
  automatically with NATS, doc requirement).
- **80.11.e** — Bridge protocol responses (`shutdown_request` /
  `plan_approval_request` JSON shapes) — niche.
- **80.11.f** — main.rs tool registration wiring (1-line snippet
  wrapping `AgentRegistry::list_by_kind` para el lookup).

**Original spec for follow-ups** (preserved):

KAIROS's `UDS_INBOX` is a Unix socket per session that lets
sibling sessions message each other (`bridge://` scheme in
`SendMessageTool`). NATS supersedes UDS, so we publish on
`agent.inbox.<goal_id>` (already used by Phase 67 driver-loop)
and add an LLM-facing tool + CLI command on top.

**Reference (PRIMARY)**: `claude-code-leak/src/utils/concurrentSessions.ts:86`
(`messagingSocketPath` PID-file field) +
`claude-code-leak/src/tools/SendMessageTool/SendMessageTool.ts:586,631,658,685,742`
(`bridge://` scheme branches).
**Reference (secondary)**: extend
`crates/core/src/team_message_router.rs` with a
`peer_inbox(goal_id)` subject helper; new
`crates/core/src/agent/list_peers_tool.rs` +
`send_to_peer_tool.rs`.

**Done criteria**:
- `agent.inbox.<goal_id>` subject contract documented in
  `docs/src/architecture/nats-subjects.md`
- `ListPeers` LLM tool returns
  `[{ goal_id, kind, channel, name, age_secs, prompt_summary }]`
- `SendToPeer { goal_id, message, status }` LLM tool publishes
  on the peer's inbox; returns `{ delivered_at, ack: true|false }`
- `nexo agent peers` CLI (mirrors `claude peers`) — alias of
  `nexo agent ps --me=false`
- 4 unit tests covering each subject path

#### 80.12 — Generic webhook receiver (provider-agnostic)   ✅ MVP

**Shipped (verification + extraction primitives)**: nuevo crate
`crates/webhook-receiver/` (~700 LOC + 33 tests verde) con
primitivas pure-fn para verificación de firmas + extracción de
event kind + render de NATS subject template. Operator
configures `WebhookSourceConfig` per source via YAML; el crate
ships los building blocks, el HTTP listener integration queda
deferida (operator wires via `axum`, hyper, o el existing `:8080`
health server).

`WebhookSourceConfig` carga 5 campos: `id` (stable identifier),
`path` (HTTP path), `signature: SignatureSpec` (algorithm +
header + prefix + secret_env), `publish_to` (NATS subject
template con `${event_kind}` substitution), `event_kind_from:
EventKindSource` (header.NAME o body.json-path), opcional
`body_cap_bytes` (default 1 MB). YAML schema con `serde(rename_all
= "kebab-case")` para algorithm enum + `tag = "kind"` para
EventKindSource — operator escribe natural YAML.

Algoritmos soportados (decision table data-driven, fácil de
extender): `HmacSha256`, `HmacSha1`, `RawToken`. Verificación
constant-time via `subtle::ConstantTimeEq` para resistir timing
attacks. Hex decoding defensive (garbage hex → InvalidSignature
sin panic). Per-spec prefix stripping (e.g. `"sha256="` antes
de hex-decode).

Pure-fn primitivas exportadas:
- `verify_signature(spec, secret, header_value, body) ->
  Result<(), RejectReason>` — constant-time HMAC compare /
  raw-token compare.
- `extract_event_kind(source, headers, body) ->
  Result<Option<String>, RejectReason>` — case-insensitive
  header lookup o JSON dotted-path body navigation
  (`json_get_dotted` recursivo, no unwraps).
- `render_publish_topic(template, event_kind) -> String` —
  template substitution, forward-compatible para variables
  futuras.

`WebhookHandler::handle(headers, body) -> Result<HandledEvent,
RejectReason>` orquesta los 4 gates en orden:
1. Body cap (rejects ANTES de cualquier HMAC compute para
   defensa DoS).
2. Signature header presente + secret env set + signature match.
3. Event kind extracción (header lookup o body JSON path).
4. Subject safety check (rechaza event_kind con `.`, `*`, `>`,
   whitespace que romperían NATS subject parsing).

Output: `HandledEvent { source_id, event_kind, topic, payload:
serde_json::Value }`. Body normalmente parsea como JSON; bodies
no-UTF-8 se envuelven como `{ "raw_base64": "..." }` — operator
sigue viendo el contenido para post-mortem sin assumir JSON.

`RejectReason` typed via `thiserror` con 7 variantes:
`OversizedBody`, `MissingSignatureHeader`, `InvalidSignature`,
`SecretMissing`, `MissingEventKind`, `InvalidBodyJson`,
`InvalidEventKindForSubject`. Caller mapea a HTTP status (401
para signature errors, 413 para oversize, 422 para missing
event kind, 500 para secret-missing operator misconfig).

`WebhookHandler::validate(config)` ejecuta boot-time invariants:
id non-empty, path non-empty + starts with `/`, publish_to
non-empty, signature.header non-empty, signature.secret_env
non-empty, body_cap_bytes > 0 cuando set, event_kind_from
header.name o body.path non-empty.

33 unit tests verde cubren:
- 4 validate tests (well-formed + 3 rejection arms)
- 6 verify_signature tests (HMAC-SHA256 match/mismatch,
  HMAC-SHA1 match, RawToken match/mismatch, garbage hex)
- 5 extract_event_kind tests (header case-insensitive, header
  missing returns None, body top-level, body nested,
  body missing path returns None, body invalid JSON errors)
- 2 render_publish_topic tests (with var, without var)
- 3 is_event_kind_subject_safe tests (rejects dot, rejects
  wildcards/whitespace/empty, accepts alphanumeric+dashes)
- 6 handle integration tests (oversized body / missing sig
  header / secret unset / invalid sig with secret set /
  happy path with publish event / event kind con dot rejected)
- 1 non-JSON body wrapping test (raw_base64 fallback)
- 2 YAML round-trip tests (full config con header extraction +
  body-path extraction shape)

Workspace `Cargo.toml::members` añadida `crates/webhook-receiver`.
Provider-agnostic por construcción: cero código GitHub-específico
o de cualquier provider; nuevos providers añaden YAML entry sin
cambio Rust.

**Three-pillar audit**: robusto (33 tests; constant-time HMAC
compare; body cap antes de HMAC defensa DoS; defensive hex
decode + JSON parse; structured RejectReason no panic; subject
safety check; YAML validation boot-time fail-fast), óptimo
(pure-fn primitivas zero-alloc en hot path; single HMAC state
allocation; Arc<WebhookSource> shareable; case-insensitive
header lookup vía ASCII lower-case sin allocations significant),
transversal (cero LLM-provider touchpoints; pure HTTP-receiver
primitive; data-driven decision table extensible per nuevos
providers via YAML).

**Wiring point** (operator opts in cuando ready):
1. Construct `let handler = WebhookHandler::new(config)` per
   source en boot.
2. HTTP listener (Axum/Hyper/health-server route): on
   `POST /webhooks/<source_id>`, collect headers + body bytes
   y llamar `handler.handle(&headers, body)`.
3. Mapear `Result<HandledEvent, RejectReason>` a HTTP response
   (200 para Ok, status apropiado para errores).
4. En Ok branch: `broker.publish(handled.topic, Event::new(...))`
   para que cualquier subscriber NATS consuma.

**Deferred follow-ups**:
- **80.12.b** — HTTP listener integration: route
  `/webhooks/<source_id>` via existing `:8080` health server o
  dedicated dispatch port (reuse `read_http_path` /
  `write_http_response` helpers para no introducir axum/hyper
  dep).
- **80.12.c** — Tunnel registration para public URL (pairs con
  `crates/tunnel/`).
- **80.12.d** — INVENTORY entries para per-source secrets
  (`WEBHOOK_<SOURCE_ID>_SECRET`) so `nexo setup doctor
  capabilities` lists them.
- **80.12.e** — Audit log per request (Phase 72-style) so
  operators can replay.
- **80.12.f** — Multi-source config validation at boot (reject
  duplicate paths or duplicate ids).
- **80.12.g** — Replay protection (idempotency tokens / nonce
  window per source).
- **80.12.h** — main.rs hookup (route + listener registration +
  per-source handler map).

**Original spec for follow-ups** (preserved):

**Re-scoped 2026-04-30**: original "KAIROS_GITHUB_WEBHOOKS — github
plugin + receiver" descopado por Cristian. Reemplazado por receiver
genérico — sin lógica GitHub-específica, sin `github_subscribe` tool,
sin event router de `pull_request|issue_comment|push|workflow_run`.
Sólo infraestructura de "webhook entrante con verificación de firma",
desacoplada de cualquier provider.

The leak's `SubscribePRTool` is GitHub-specific (DCE'd body in leak).
We ship the receiver primitive only — providers (GitHub/Stripe/Linear/
Sentry/anything) wire on top via YAML config, no per-provider Rust
code. Channels (Phase 6/26 + 80.9) and pollers (Phase 19) remain the
primary inbound paths; webhooks complement when provider supports
push semantics.

**Reference (PRIMARY)**: `claude-code-leak/src/tools.ts:48`
(`SubscribePRTool` gate — DCE'd, signature visible only).
**Reference (secondary)**: existing `crates/tunnel/` for HTTP
exposure; existing `crates/broker/` for fan-out via NATS subject;
existing `crates/poller/` for downstream consumers.

**Done criteria**:
- New `crates/webhook-receiver/` exposes:
  - HTTP receiver behind existing tunnel (Phase 26): `POST /webhooks/<source_id>`
  - Per-source config block in `agents.yaml`:
    ```yaml
    webhook_receiver:
      sources:
        - id: my-github
          path: /webhooks/github
          signature:
            algorithm: hmac-sha256
            header: X-Hub-Signature-256
            prefix: "sha256="
            secret_env: GITHUB_WEBHOOK_SECRET
          publish_to: webhook.github.${event_kind}  # NATS subject template
          event_kind_from: header.X-GitHub-Event    # how to extract event_kind from request
        - id: stripe-prod
          path: /webhooks/stripe
          signature:
            algorithm: hmac-sha256
            header: Stripe-Signature
            prefix: ""
            secret_env: STRIPE_WEBHOOK_SECRET
          publish_to: webhook.stripe.${event_kind}
          event_kind_from: body.type               # or `body.<json-path>`
    ```
  - Signature verifier supports HMAC-SHA256 + HMAC-SHA1 + raw-token
    (config-driven; no provider-specific arms)
  - Event-kind extraction supports `header.<Name>` or
    `body.<json-path>` (config-driven)
  - Verified payload published to NATS subject from `publish_to`
    template (`${event_kind}` substituted)
  - Body cap (default 1 MB) + per-source rate limit (Phase 76.5
    pattern) + audit log (Phase 72 pattern, optional)
- `crates/setup/src/capabilities.rs::INVENTORY` registers per-source
  `WEBHOOK_<SOURCE_ID>_SECRET` env vars (operator-side gate visibility)
- 4 unit tests: HMAC-SHA256 verify, HMAC-SHA1 verify, signature
  mismatch → 401, missing signature header → 401, oversized body → 413,
  event-kind extraction from header vs body
- 1 integration test using mock HTTP request fixture

**Out of scope (vs original)**:
- ~~GitHub event router (`pull_request|issue_comment|push|workflow_run`)~~
- ~~`github_subscribe` LLM tool~~
- ~~`crates/plugins/github/`~~
- Provider-specific subscriber YAML + prompt templates — these belong
  in user config or downstream poller, NOT in the receiver crate

**Effort revised**: ~1.5-2 d (was 3 d).

#### 80.13 — KAIROS_PUSH_NOTIFICATION — APN/FCM/WebPush tool   ⬜

KAIROS gates `PushNotificationTool` (DCE'd body, signature
visible in `tools.ts:46`). Distinct from `notify_origin`:
push is a one-way mobile alert when a goal completes or
awaits user input; doesn't reuse the conversational channel.

**Reference (PRIMARY)**: `claude-code-leak/src/tools.ts:45-50`
+ `BriefTool.ts:139` (alias `'PushNotification'` legacy).
**Reference (secondary)**: new
`crates/plugins/push/` with `PushProvider` trait + APN, FCM,
WebPush impls; per-binding credentials via Phase 17
nexo-auth.

**Done criteria**:
- `PushProvider` trait (`send(title, body, payload, recipient)`)
- 3 impls: APN (token-based, p8 key), FCM (HTTP v1 with
  service account JSON), WebPush (VAPID)
- LLM tool `notify_push { title, body, payload?, recipient_id? }`
  — `recipient_id` defaults to current binding's primary
  registration
- Config block per-binding:
  ```yaml
  push:
    provider: apn|fcm|webpush
    credentials_ref: secrets/push-prod.json
    default_recipient: <device_id>
  ```
- `nexo agent push test --binding=<id>` CLI smoke
- 4 unit tests (one per provider + tool wiring)

#### 80.14 — AWAY_SUMMARY — re-connection digest   ✅ MVP

**Shipped (template-based)**: nuevo
`crates/config/src/types/away_summary.rs` (~120 LOC + 6 unit tests)
con `AwaySummaryConfig { enabled, threshold_hours, max_events }` +
`validate()` que rechaza threshold > 30 días y max_events == 0.
`AgentConfig.away_summary: Option<AwaySummaryConfig>` con
`#[serde(default)]` para backward-compat. Workspace fixture sweep
agregó `away_summary: None` a 49+ struct literales después de
`assistant_mode: None`. `TurnLogStore::tail_since(since, limit)`
helper trait method (default impl returns empty para safety),
override en `SqliteTurnLogStore` que ejecuta
`SELECT ... WHERE recorded_at >= ?1 ORDER BY recorded_at DESC
LIMIT ?2` (cap to TAIL_HARD_CAP=1000). Nuevo
`crates/dispatch-tools/src/away_summary.rs` (~280 LOC + 11 tests
verde) con: (1) `try_compose_away_digest(cfg, last_seen, now, log)
-> Result<Option<String>, AwaySummaryError>` función pure-async
con 4 gates ordenados (enabled / last_seen present / elapsed >=
threshold / log non-empty); (2) `build_digest(events, elapsed,
max_events) -> String` pure-fn renderer template-based markdown
con counters de completed / aborted_cancelled / failed / other +
truncation hint cuando hits max_events cap; (3) `AwaySummaryError`
typed wrapper sobre `AgentRegistryStoreError`. Re-exports en
`nexo-dispatch-tools::lib.rs`: `try_compose_away_digest`,
`build_digest`, `AwaySummaryError`. Output ejemplo:

```
**While you were away** (last 6h0m):
- 3 goal turn(s) recorded
- 2 completed
- 1 failed
```

11 tests cubren: disabled-returns-none, last-seen-none-returns-none
(bootstrap path), elapsed-below-threshold-returns-none,
negative-elapsed-returns-none (clock skew), empty-log-returns-none,
populated-log-returns-digest (verifica markdown contains expected
counters), digest-renders-completed-aborted-failed-counts (mixed
outcomes), digest-caps-at-max-events (truncation suffix),
digest-below-cap-no-truncation-suffix, digest-renders-minutes-
correctly (2h30m format), populated-log-truncates-to-max-events
(integration via mock log). Mock `MockLog` impl `TurnLogStore`
permite scripted records sin SQLite real para tests deterministas.

**Three-pillar audit**: robusto (11 tests; 4 gates ordenados
cheapest-first; defensive parsing en config validate; fail-safe
fallback impl en trait default; UTC throughout), óptimo (pure-fn
template render zero-LLM-call; SQLite query indexed via WHERE
+ LIMIT; mock-based tests sin DB real), transversal (zero
LLM-provider touchpoints — pure markdown template; works under
Anthropic/MiniMax/OpenAI/Gemini/DeepSeek/xAI/Mistral igual).

**Wiring point** (operator opts in cuando ready): inbound handler
debe llamar `try_compose_away_digest(...)` antes de procesar el
mensaje del usuario; si retorna `Some(digest)`, lo entrega via
`notify_origin` (existing) y luego procesa el inbound. Operator
mantiene `last_seen_at` per (channel, sender_id) en el storage de
su elección — el helper acepta el timestamp como parameter, sin
acoplar la implementación a `nexo-pairing` o tabla específica.

**Deferred follow-ups**:
- **80.14.b** — LLM-summarised version: forks subagent que toma
  los events + builds 1-3 sentence summary natural (estilo
  upstream pattern). Hoy MVP es template-based; LLM version
  requiere fork-orchestration similar a Phase 80.1.
- **80.14.c** — `last_seen_at` tracking en `nexo-pairing::PairingStore`
  con SQLite migration (slim MVP delega a caller storage).
- **80.14.d** — Per-channel-adapter delivery (whatsapp/telegram
  render markdown distinto).
- **80.14.e** — Time-of-day awareness ("don't ping at 3am").
- **80.14.f** — Custom prompt template per agent (cuando 80.14.b
  shippea).
- **80.14.g** — main.rs inbound interceptor wire (1-line
  invocation site, blocked on dirty-state pattern).

**Original spec for follow-ups** (preserved):

When a user comes back after a long gap, KAIROS gates an
"away digest" generation. Leak's body is DCE'd
(`AWAY_SUMMARY` flag only) — design from scratch using the
extractMemories pattern.

**Reference (PRIMARY)**: `claude-code-leak/src/main.tsx`
search hits for `AWAY_SUMMARY` (DCE'd; only the integration
point survives — verify in 80.0 inventory).
**Reference (secondary)**: extend
`crates/pairing/` with `last_seen_at` per (channel, account)
+ new `crates/core/src/agent/away_summary.rs`.

**Done criteria**:
- `pairing_store` schema gains `last_seen_at: i64` per
  (channel, account_id)
- On inbound message: compute `now - last_seen_at` —
  if `> threshold` (default 1 h, configurable per binding):
  spawn forked goal that summarises Goals completed /
  aborted / `notify_origin`s fired / cron fires while away
  via Phase 72 turn-log query
- Digest delivered as a single message before the user's
  inbound is processed (or skipped if empty)
- Configurable threshold, max digest length (token-budget),
  off by default
- 3 unit tests + 1 integration: simulate 2 h offline → digest
  emitted; 5 min offline → no digest

#### 80.15 — Assistant module: kairos-active flag + system addendum + initial team   ✅ MVP

**Shipped**: nuevo crate `nexo-assistant` (~150 LOC + 6 unit tests)
exponiendo `AssistantConfig` + `ResolvedAssistant::resolve()` +
`DEFAULT_ADDENDUM` const con texto proactivo provider-agnostic.
`crates/config/src/types/assistant.rs` (~140 LOC + 7 unit tests)
shippea el YAML schema con `enabled` + `system_prompt_addendum`
optional + `initial_team: Vec<String>` + `validate()` que rechaza
addendum vacío y team-names malformados (alphanum+`-`+`_` only).
`AgentConfig.assistant_mode: Option<AssistantConfig>` con
`#[serde(default)]` para backward compat. `AgentContext.assistant:
ResolvedAssistant` field nuevo, default `disabled()` en el
constructor — fixtures siguen compilando sin opt-in. Workspace
sweep `perl -i -pe` añadió `assistant_mode: None` a 49 struct
literals across `crates/{core,fork,dream}` + `src/main.rs` +
`crates/core/tests/`. **System-prompt injection wired** en
`crates/core/src/agent/llm_behavior.rs` después del proactive +
coordinator hints: cuando `ctx.assistant.should_append_addendum()`
(boot-immutable enabled ∧ non-empty addendum), el texto se push
a `channel_meta_parts` con cache-friendly stable ordering
(prompt-cache stays warm across turns). Tests verde:
nexo-assistant 6, nexo-config types::assistant 7, nexo-core build
green, workspace build green. **Three-pillar audit**: robusto
(default-disabled, validate rejects empty/malformed, boot-immutable
flag avoids mid-turn race, `Arc<String>` shared addendum no
per-turn alloc, 13+ tests cover all gates), óptimo (resolved once
at boot, single byte bool, `Arc<Vec>` shared team list, fixture
sweep avoids per-call alloc), transversal (provider-agnostic
default text — no Anthropic/OpenAI/Gemini/MiniMax/DeepSeek/xAI/
Mistral phrasing — bool readable by any consumer, addendum is
plain English nudge toward proactive posture).

**Deferred to follow-ups**:
- **80.15.b** — `initial_team` auto-spawn at boot (depends on
  Phase 8 agent-to-agent + 80.10 BG sessions).
- **80.15.c** — auto-flip `cron.enabled: true` default for
  assistant bindings (depends on 80.6 killswitch).
- **80.15.d** — auto-flip `brief: true` default (depends on 80.8
  SendUserMessage tool).
- **80.15.e** — activation-path telemetry / provenance.
- **80.15.f** — `nexo setup doctor` per-binding `assistant_mode`
  reporter row (low priority, polish).
- **80.15.g** — `src/main.rs` boot wiring populating
  `ctx.assistant = ResolvedAssistant::resolve(cfg.assistant_mode.as_ref())`
  (1-line snippet documented in `crates/assistant/src/lib.rs`
  doc-comment; deferred until user resolves their existing dirty
  state per the 80.1.b.b.b / 80.1.c / 80.1.d / 80.1.e / 80.1.g
  pattern).

Until 80.15.g lands, the boolean stays `false` for every binding
at runtime — the system-prompt addendum is invisible. The
infrastructure ships ready; flipping the boot-side wiring on is
a 1-line opt-in.

**Original spec for follow-ups** (preserved for reference):

KAIROS is gated by an `assistantModule` import + `kairosEnabled`
runtime boolean computed at REPL startup. When active: a
system-prompt addendum is appended, `assistantTeamContext`
pre-seeds in-process teammates, `fullRemoteControl` is
implied (80.17). nexo equivalent: per-binding
`assistant_mode: true` flag drives the same set of effects.

**Reference (PRIMARY)**: `claude-code-leak/src/main.tsx:1058-1088`
(activation), `:1075` (gate computation),
`:2206-2208` (system prompt addendum),
`:3035` (teamContext precedence),
`:2962, 3334` (initialState injection).
**Reference (secondary)**: extend
`crates/config/src/types/agents.rs` with
`AgentBinding::assistant_mode` + `crates/core/src/agent/effective.rs`
threads it into runtime + `crates/llm/src/prompt_assembly.rs`
appends the addendum.

**Done criteria**:
- Binding YAML knob: `assistant_mode: true|false` (default `false`)
- When `true`:
  - System prompt addendum appended (text identical to leak's,
    paraphrased to drop Anthropic-specific wording — confirm
    in 80.0)
  - Initial team auto-spawned per binding's `team:` block
  - Brief mode (80.8) auto-on
  - 80.6 cron killswitch defaults `enabled: true`
- 3 unit tests (off path, on path, mid-session toggle via
  hot-reload)

#### 80.16 — `nexo agent attach` + `nexo agent discover`   ✅ MVP

**Shipped (DB-only viewer)**: 2 nuevos `Mode::AgentAttach
{ goal_id, db, json }` y `Mode::AgentDiscover
{ include_interactive, db, json }` en `src/main.rs::Mode` + 2 parser
arms (`agent attach <goal_id>` con UUID en posicional, `agent
discover [--include-interactive]`) + 2 dispatch arms + 2 async run
fns (~150 LOC total). **`run_agent_attach`** valida UUID upfront
(exit 1 con clean error si bad shape), abre store via
`resolve_agent_db_path`, fail si DB ausente, fetch handle via
`store.get(GoalId(uuid))`, error "no agent handle found" si row no
existe. Render markdown con campos: goal_id full / kind / status /
phase_id / started_at / finished_at / last_progress_text /
last_diff_stat / turn_index/max_turns / last_event_at. Hint final
diferenciado: si `Running` → "Live event stream requires daemon
connection (Phase 80.16.b follow-up)"; si terminal → "Goal is in
terminal state X; no further updates expected". `--json` path
serializa el handle completo via `serde_json::to_string_pretty`.
**`run_agent_discover`** acepta flag `--include-interactive` que
broadens kinds to all 4 variants (default sólo Bg/Daemon/DaemonWorker
— el "qué tengo corriendo detached" del operador). Itera kinds via
`store.list_by_kind` then `retain` Running, sort by `started_at`
desc. Empty result imprime "(no detached / daemon goals running;
pass --include-interactive to broaden)" con hint condicional.
Renderiza tabla markdown con cols ID / Kind / Phase / Started /
Last activity. **8 nuevos tests inline verde**:
`run_agent_attach_rejects_invalid_uuid` (bad shape → "valid UUID"),
`run_agent_attach_missing_db_errors` (--db pointing nowhere → "not
found"), `run_agent_attach_handle_not_found_errors` (valid UUID
not in DB → "no agent handle found"),
`run_agent_attach_running_renders_snapshot` (seed Running Bg via
`run_agent_run`, attach succeeds + JSON variant),
`run_agent_discover_filters_to_bg_daemon` (seed 1 Interactive + 1
Bg, assert default discover excludes Interactive via store query),
`run_agent_discover_include_interactive_returns_all` (with flag,
both render),
`run_agent_discover_empty_db_friendly_message` (missing DB → exit 0
with friendly + JSON variant),
`run_agent_discover_no_matching_goals_renders_friendly` (seed only
Interactive, default discover prints friendly hint). CLI smoke
manual confirmed: `nexo agent discover` → tabla con bg row;
`nexo agent attach <goal_id>` → markdown + Running hint;
`nexo agent attach not-a-uuid` → exit 1 con "is not a valid UUID";
`nexo agent attach <missing-uuid>` → exit 1 con "no agent handle
found".

**Three-pillar audit**: robusto (8 tests covering UUID parse / DB
absent / handle absent / Running render / terminal render /
discover empty / discover no-match / both --json paths; defensive
flag composition; sort newest-first), óptimo (reuses
`list_by_kind` + `get` from 80.10 store; pure RO pool; helpers
`resolve_agent_db_path` + `short_uuid` shared with 80.1.d / 80.10),
transversal (pure SQLite + CLI; cero LLM-provider touchpoints).

**Deferred follow-ups**:
- **80.16.b** — Live event streaming via NATS subscribe
  (`agent.registry.snapshot.<goal_id>` + `agent.driver.>` filter
  by goal_id payload). Requires `nexo-broker` connect from CLI
  side; print events as they arrive; Ctrl-C detaches without
  killing.
- **80.16.c** — User input piping via `agent.inbox.<goal_id>`
  (depends on Phase 80.11).
- **80.16.d** — Interactive REPL UI for attach (TUI). Plain
  stdout printing covers MVP today.

**Original spec for follow-ups** (preserved):

KAIROS preprocesses argv into `_pendingAssistantChat = { sessionId, discover }`
so `claude assistant <uuid>` attaches a REPL to a running
daemon, and `claude assistant` (no arg) opens a discovery
view. CLI mirror: `nexo agent attach <goal_id>` + `nexo agent discover`.

**Reference (PRIMARY)**: `claude-code-leak/src/main.tsx:559,685-694,3259-3340`.
**Reference (secondary)**: extend
`crates/dispatch-tools/` (CLI surface) with `attach` +
`discover` subcommands; reuses Phase 67 NATS subjects for
streaming output back to the attaching TTY.

**Done criteria**:
- `nexo agent attach <goal_id>` connects current stdin/stdout
  to the live goal — typed lines route to `agent.inbox.<goal_id>`,
  goal output streams back from `agent.events.<goal_id>`
- `nexo agent discover` lists running goals with channel,
  age, prompt-summary; default sort by age desc
- Detach on Ctrl-C (matches isBgSession exit-path semantics)
- Companion-tui pairing flow piggy-backs on the same subjects
- 2 unit tests (subject shape, stream framing) + 1 manual
  integration test (running daemon, attach via second
  shell)

#### 80.17 — `auto_approve` mode (curated auto-approve dial)   ✅ MVP

**Renamed from `kairos_remote_control` to `auto_approve` per
operator request — descriptive name, no codename attribution.**

**Shipped (decision table)**: nuevo módulo
`crates/driver-permission/src/auto_approve.rs` (~280 LOC + 27
tests verde) con `pub fn is_curated_auto_approve(tool_name, args,
auto_approve_on, workspace_path) -> bool`. Decision table cubre
~25 entries:

- **Always auto** (read-only / info gathering): `FileRead`, `Glob`,
  `Grep`, `LSP`, `list_agents`, `agent_status`, `agent_turns_tail`,
  `memory_history`, `dream_runs_tail`, `list_mcp_resources`,
  `read_mcp_resource`, `WebFetch`, `WebSearch`, `list_followups`,
  `list_peers`, `task_get`.
- **Bash conditional**: pasa solo si `is_read_only` AND
  `check_destructive_command.is_none()` AND `check_sed_in_place.is_none()`.
  Defense-in-depth — Phase 77.8/77.9 destructive heuristics SIEMPRE
  vetan (rm -rf, dd, mkfs, sed -i, etc).
- **FileEdit/FileWrite conditional**: pasa solo si la ruta
  canonicalizada (con fallback a parent.canonicalize() para archivos
  nuevos) cae bajo `workspace_path`. Symlink-escape resistant.
- **Notifications + memory + coordination**: `notify_origin`,
  `notify_channel`, `notify_push`, `forge_memory_checkpoint`,
  `dream_now`, `ask_user_question`, `delegate`, `team_create`,
  `team_delete`, `send_to_peer`, `task_create`, `task_update`,
  `task_stop`.
- **NEVER auto**: `ConfigTool`, `config_self_edit`, `REPL`,
  `remote_trigger`, `schedule_cron`. Aunque el dial esté on, estos
  siempre piden interactivo.
- **`mcp_*` / `ext_*` prefix**: default-ask (heterogéneo).
- **Default arm `_ => false`**: tools nuevos requieren explicit
  add al match para auto-aprobar.

`AgentConfig.auto_approve: bool` con `#[serde(default)]` (default
`false` = comportamiento actual sin cambios). Per-binding override
`InboundBinding.auto_approve: Option<bool>` también con default.
`EffectiveBindingPolicy` gana 2 fields: `auto_approve: bool`
resuelto via override > agent default, y `workspace_path:
Option<PathBuf>` derivado de `agent.workspace`. Workspace fixture
sweep: 49 sites con `assistant_mode: None` + 14 sites con
`repl: None` swept via 2 perl multi-line replaces para añadir
`auto_approve: false` y `auto_approve: None` a struct literals
respectivamente.

**Tests verde**: 27 nuevos en `auto_approve::tests` cubriendo cada
arm del match + variantes defensivas (disabled flag → all false /
file_read auto / glob_grep_lsp auto / bash ls auto / bash rm-rf
veta / bash sed-in-place veta / bash missing arg defensive /
bash pipe with destructive vetoes / FileEdit inside workspace
auto / FileEdit outside workspace veta / FileEdit no workspace
config blocks / FileEdit new file uses parent canonicalize /
FileEdit missing path arg / notify_origin auto / notify_push
auto / dream_now auto / delegate auto / team_create auto /
task_create auto / task_get auto read-only / ConfigTool never
auto / REPL never auto / remote_trigger never auto / schedule_cron
never auto / mcp_ prefix default ask / ext_ prefix default ask /
unknown tool default ask). `cargo build --workspace` + `cargo
test --bin nexo` verde post-change.

**Three-pillar audit**: robusto (27 tests; default-deny match arm
para tools nuevos; Phase 77.8/77.9 vetoes preservados; canonical-
path symlink defense; defensive arg-missing → false; per-binding
override resolution); óptimo (pure fn single-match zero-allocation
hot path; reuses existing classifiers; helper canonical_starts_with
private to module); transversal (cero LLM-provider touchpoints;
pure decision over (name, args, policy); transversal Anthropic /
MiniMax / OpenAI / Gemini / DeepSeek / xAI / Mistral).

**Deferred follow-ups**:
- **80.17.b** — Hook `is_curated_auto_approve` into the approval
  gate ✅ **MVP** — shipped: `AutoApproveDecider<D>` decorator
  envuelve cualquier `PermissionDecider` inner; lee
  `auto_approve: bool` + `workspace_path: String` del campo
  `request.metadata` (defensive `as_bool()` / `as_str()` —
  missing/wrong-type → false → delegate); cuando
  `is_curated_auto_approve` retorna true, short-circuita a
  `AllowOnce { updated_input: None }` con rationale
  `"auto_approve: curated subset (<tool_name>)"`; cuando false,
  delega al inner decider preservando comportamiento normal.
  Constants `META_AUTO_APPROVE = "auto_approve"` y
  `META_WORKSPACE_PATH = "workspace_path"` exportadas para que
  caller-side wiring use los mismos field names. 6 nuevos tests
  en `decorator_tests` mod cubriendo: delegates-when-metadata-missing
  (Inner DenyAll → Deny pasa), delegates-when-flag-false (auto_approve:
  false → delega), short-circuits-for-curated-tool (auto_approve:
  true + FileRead → AllowOnce sin tocar inner), delegates-for-
  destructive-bash (auto_approve: true + `rm -rf` → helper rejects
  → delega a Inner AllowAll), delegates-for-unknown-tool (default-
  ask para nuevos tools), handles-string-in-bool-field-defensively
  (`"true"` string → `as_bool()` returns None → false → delega).
  Re-exports en `nexo-driver-permission::lib.rs`:
  `AutoApproveDecider`, `META_AUTO_APPROVE`, `META_WORKSPACE_PATH`,
  `is_curated_auto_approve` (último por completitud — 27 tests
  inventory + 6 tests decorator = 33 tests verde totales en
  módulo). Doc-comment del decorator incluye snippet de wiring
  caller-side de 1-line para boot:
  `let decider = AutoApproveDecider::new(inner)` + ejemplo de
  metadata population from `EffectiveBindingPolicy`. Cero cambios
  a `PermissionRequest` shape, cero changes a `mcp.rs` o `socket.rs`
  o decider impls existentes. Caller-side metadata population
  queda como **80.17.b.c** follow-up (siguiente sub-fase): el wire
  que CONSTRUYE `PermissionRequest` (probablemente
  `crates/driver-claude/` o el adapter MCP) debe insertar
  `auto_approve` + `workspace_path` desde la
  `EffectiveBindingPolicy` resuelta antes de invocar al decider.
  Sin esa población, el flag siempre lee `false` desde metadata
  y el decorator es transparent pass-through. Three-pillar audit:
  **robusto** (6 tests; defensive metadata parsing; decorator
  preserves inner behavior cuando flag off; rationale string con
  tool name para audit trail), **óptimo** (zero allocations en
  hot path cuando flag off; single helper invocation; metadata
  read es trivial JSON access), **transversal** (decorator funciona
  con cualquier PermissionDecider impl; cero LLM-provider
  touchpoints; pure pre-decision filter). Operator wiring (boot-
  time wrap) deferred as 80.17.b.b cuando el dirty state del user
  resuelva.

- **80.17.b.b** — main.rs wire: cuando el operador resuelva su
  dirty state, wrap `AllowAllDecider` (or future LlmDecider) con
  `AutoApproveDecider::new(Arc::clone(&inner))`. 1-line snippet
  en doc-comment.

- **80.17.b.c** — Caller-side metadata population: ensure que el
  wire que CONSTRUYE `PermissionRequest` (probablemente en
  `crates/driver-claude/` o adapter MCP) populate
  `metadata.auto_approve` + `metadata.workspace_path` desde
  `EffectiveBindingPolicy` antes de invocar al decider.
- **80.17.c** — `nexo setup doctor` warn check:
  `assistant_mode == true && auto_approve == false` ⇒ "agent will
  hang on every tool call waiting for interactive approval".
- **80.17.d** — Per-tool log line on AutoAllow path (audit trail
  "what was auto-approved this turn"); routes to Phase 72 turn-log.
- **80.17.e** — Operator runtime override `--no-auto-approve`
  CLI flag forcing ask even when YAML says auto.
- **80.17.f** — Customisable allowlist via YAML (operator
  extends/restricts the curated subset).

**Original spec for follow-ups** (preserved):

`fullRemoteControl = remoteControl || getRemoteControlAtStartup() || kairosEnabled`
(`main.tsx:2916`). When on, approval prompts are bypassed for
a curated tool subset. Critical for unattended daemons. Must
not bypass the Phase 16 capability gate — the gate is
*authoritative*; `kairos_remote_control` only flips the
auto-approve dial within whatever the gate already permits.

**Reference (PRIMARY)**: `claude-code-leak/src/main.tsx:2916`.
**Reference (secondary)**: extend
`crates/config/src/types/agents.rs::BindingPolicy` with
`kairos_remote_control: bool`; consumed in
`crates/driver-permission/`.

**Done criteria**:
- New binding-policy field — default `false`
- When `true` AND tool is in `auto_approve_tools` allowlist:
  permission auto-approves; otherwise normal flow
- `auto_approve_tools` defaults to: `Sleep`, `cron_*`,
  `notify_*`, read-only `Bash`, `FileRead`, `Grep`, `Glob`
- `nexo setup doctor` warns when `assistant_mode: true` AND
  `kairos_remote_control: false` (likely misconfiguration)
- 4 unit tests across the matrix (mode on / off × tool
  in / out of allowlist)

#### 80.18 — DreamTask audit-log row   ✅

Shipped — `crates/agent-registry/src/dream_run.rs` (~860 LOC + 26
unit tests). Verbatim port of leak `tasks/DreamTask/DreamTask.ts:1-158`.
Mirrors Phase 72 turn-log pattern (`turn_log.rs:1-336`).

**Public surface**:
- `DreamRunStore` trait (10 async methods).
- `SqliteDreamRunStore` impl with idempotent `migrate` (v4) + 3 indexes.
- Types: `DreamRunRow`, `DreamRunStatus` (5 variants), `DreamPhase`
  (2 variants), `DreamTurn { text, tool_use_count }`.
- Constants: `MAX_TURNS=30` (from leak `:11-12`), `TAIL_HARD_CAP=1000`.

**Three pillars verified**:
- **Robusto**: 12 risks enumerated; idempotent `INSERT OR IGNORE` on
  `(goal_id, started_at)`; transactional `append_turn` + `append_files_touched`
  with `BEGIN`/`COMMIT`; MAX_TURNS server-side cap (test inserts 35,
  asserts 30 retained); TAIL_HARD_CAP defends `tail(usize::MAX)`;
  `Option<i64>` distinguishes `Some(0)` from `None` for
  `prior_mtime_ms`; silent `Ok(())` on update of missing id;
  `busy_timeout=5s` on `SqliteConnectOptions`; reattach atomic
  single-UPDATE.
- **Óptimo**: copy/paste pool config from Phase 72 (`WAL` + `synchronous=NORMAL`
  + max_connections heuristic); JSON columns avoid 2 join tables;
  shared `SqlitePool` interoperable with `TurnLogStore` /
  `AgentRegistryStore`; 3 indexes (`goal_id`, `started_at`, `status`)
  cover every query path.
- **Transversal**: store has zero `LlmClient` coupling — `DreamTurn`
  is plain text + count; `fork_label: String` flexible (supports
  `auto_dream`, `away_summary`, future eval); admin-ui (TS) reads
  same JSON shape.

**Decisions taken**:
- Crate placement `crates/agent-registry/src/dream_run.rs` (mirror
  Phase 72) — NOT `crates/dream-store/`.
- `fork_label: String` flexible — no enum variant churn.
- `turns` JSON column inside the row — bounded by MAX_TURNS=30; no
  separate `dream_turns` join table.
- Phase 71 reattach extension to call `dream_run_store.reattach_running()`
  is **80.18.b follow-up** — API ready here, integration in
  `reattach.rs` deferred until 80.1 wires the store at boot.
- Concurrent-writers test scoped to distinct rows (production
  pattern — one fork = one writer per row); same-id contention
  test removed because sqlx 0.8 returns SQLITE_BUSY on `BEGIN IMMEDIATE`
  contention even with `busy_timeout` configured (known
  sqlx-sqlite limitation; Phase 72 sidesteps the same way).

**Files shipped**:
- `crates/agent-registry/src/dream_run.rs` (NEW)
- `crates/agent-registry/src/lib.rs` re-exports `DreamRunStore`,
  `SqliteDreamRunStore`, types, `DREAM_MAX_TURNS`,
  `DREAM_TAIL_HARD_CAP`.

**Tests** (26 total, all green):
- 2 — enum serde round-trip (`DreamRunStatus`, `DreamPhase`).
- 1 — migrate idempotent across `open` calls + within same pool.
- 6 — insert + get + tail (round-trip, idempotent UNIQUE constraint,
  newest-first ordering, hard-cap clamp, isolation by goal_id, missing-id None).
- 4 — update_status + update_phase + finalize (in-place modify,
  silent on missing id, finalize sets ended_at without touching status).
- 3 — append_files_touched (dedupe, empty no-op, all-duplicates no-op).
- 3 — append_turn (skip empty no-op per leak `:87-92`, append when
  text present, trim to MAX_TURNS=30 after 35 inserts).
- 5 — reattach + drop (running→lost flip, idempotent re-run, goal-id
  isolation, nonexistent goal returns 0, prior_mtime_zero distinguished
  from None).
- 2 — sequential cross-row writers don't interfere; migrate idempotent
  across re-`open`.

**Follow-ups** (`proyecto/FOLLOWUPS.md`):
- 80.18.b — `crates/agent-registry/src/reattach.rs::reattach()`
  extension to call `dream_run_store.reattach_running()` when
  store wired at boot (~30 min plumbing).

KAIROS's `DreamTask` state shape (`DreamTask.ts:25-41`):
`{ type, status, phase, sessionsReviewing, filesTouched,
turns(max 30), priorMtime, abortController }`. nexo equiv:
extend `agent_registry::TurnLogStore` (Phase 72) with a
typed `dream_run` row joined to `goal_id`, queryable via the
same `agent_turns_tail` shape.

**Reference (PRIMARY)**: `claude-code-leak/src/tasks/DreamTask/DreamTask.ts:25-130`.
**Reference (secondary)**: extend
`crates/agent-registry/` with `dream_runs` table
(migration v4 — Phase 77.17 system).

**Done criteria**:
- New table:
  ```sql
  CREATE TABLE dream_runs (
      id TEXT PRIMARY KEY,
      goal_id TEXT NOT NULL,
      status TEXT NOT NULL,    -- running|completed|failed|killed
      phase TEXT NOT NULL,     -- starting|updating
      sessions_reviewing INTEGER NOT NULL,
      files_touched TEXT NOT NULL,  -- JSON array
      prior_mtime INTEGER,
      started_at INTEGER NOT NULL,
      ended_at INTEGER
  );
  ```
- `dream_runs_tail` LLM tool returns last N rows as a
  markdown table
- `dream.kill <run_id>` admin CLI sets abort signal +
  rolls back consolidation lock
- 3 unit tests

#### 80.19 — Forked subagent infra (cache-safe + skip-transcript)   ✅

Shipped — new crate `crates/fork/` (≈ 1450 LOC + 42 unit tests
across 9 modules). Standalone in-process turn loop using
`nexo_llm::LlmClient` directly — does NOT reuse Phase 67's
heavyweight goal-flow `DriverOrchestrator` (that path spawns
`claude` subprocesses and runs acceptance + workspace checks,
which is the wrong primitive for fork-with-cache-share).

**Spec amend (live)**: discovery during Step 1 audit revealed
that `nexo_llm` does not expose `Message`/`ThinkingConfig` and
that `DriverOrchestrator` is goal-flow heavyweight; spec was
revised in chat to use real types — `ChatMessage` + `ToolDef` +
`CacheUsage` from `nexo_llm::types`, and a standalone
`turn_loop::run_turn_loop` instead of the orchestrator. Two
TypeScript-only fields from the leak's `SubagentContextOverrides`
(17 total) were dropped because Rust's `Arc<...>` already
isolates by construction; only `agent_id` + `critical_system_reminder`
remain in `ForkOverrides`.

**Public surface**:
- `CacheSafeParams::from_parent_request(&ChatRequest)` snapshots
  the parent's last LLM request (system prompt, system blocks,
  tools, model, message prefix). `cache_key_hash()` lets
  Phase 77.4 cache-break detector compare parent vs fork.
- `ForkSubagent` trait + `DefaultForkSubagent` impl.
- `ForkParams` carries `mode: DelegateMode { Sync | ForkAndForget }`,
  `tool_filter: Arc<dyn ToolFilter>`, `tool_dispatcher`,
  `on_message`, `skip_transcript`, `timeout`, `external_abort`.
- `ForkHandle` exposes `take_completion()` to extract the
  completion future without breaking the `Drop` impl that
  cancels the abort signal on abandoned handles.
- `ForkResult { messages, total_usage, total_cache_usage,
  final_text, turns_executed }`.
- `OnMessage` trait + `NoopCollector` / `LoggingCollector` /
  `ChainCollector` (panic-safe via `catch_unwind`).
- `ToolFilter` trait + `AllowAllFilter` default; Phase 80.20
  ships `AutoMemFilter` against the same trait.
- `CacheSafeSlot` per-goal slot (caller-owned) for the
  most-recent params, mirror of leak's
  `lastCacheSafeParams` slot but per-goal not global static.

**Cache-key invariant (CRITICAL — leak `forkedAgent.ts:522-525`)**:
`fork_context_messages` MUST preserve incomplete `tool_use`
blocks; filtering them strips paired `tool_result` rows and
breaks Anthropic API + cache prefix. Test
`from_parent_request_preserves_message_prefix_with_partial_tool_use`
verifies bit-for-bit pass-through.

**Telemetry**: span `fork.subagent` with run_id, parent_agent,
fork_label, query_source, mode, skip_transcript, cache_key_hash.
Inline `WARN` on `fork.cache_break_detected` target when first-turn
cache hit ratio drops below 0.5 (Phase 77.4 heuristic).

**Decisions taken**:
- Crate `crates/fork/` separate from `delegation_tool.rs`
  (D-8 in `proyecto/design-kairos-port.md`) — 80.19 ships the
  primitive; `delegation_tool.rs` refactor to consume it is
  a follow-up step, not part of 80.19.
- `skip_transcript: true` does NOT register an `agent_handle`
  row (fork invisible to `agent ps`); 80.10 SessionKind +
  80.18 `dream_runs` provide consumer-specific audit.
- Cross-process `NatsForkSubagent` deferred to Phase 32
  multi-host orchestration.
- `Drop for ForkHandle` cancels abort when `take_completion`
  was never called — prevents leaked tokio tasks on abandoned
  ForkAndForget handles.

**Files shipped**:
- `crates/fork/Cargo.toml`, `crates/fork/README.md`,
  `crates/fork/src/{lib,error,delegate_mode,cache_safe,
  overrides,tool_filter,turn_loop,on_message,fork_handle,
  fork_subagent}.rs` (10 files)
- `Cargo.toml` workspace.members += `crates/fork`
- `docs/src/architecture/fork-subagent.md` registered in
  `docs/src/SUMMARY.md` under `# Architecture`

**Tests** (42 total, all green):
- 4 — error + delegate_mode types
- 9 — CacheSafeParams: preserves partial tool_use (the leak
  invariant), cache_key_hash stability, hash invariance under
  temperature drift, slot save/clear.
- 4 — overrides: agent_id override, Arc-pointer preservation
  for config + sessions.
- 1 — AllowAllFilter accepts everything.
- 7 — turn_loop: single text turn, tool_call → text round-trip,
  abort + max_turns_zero edge cases, filter denial substitutes
  the right body, critical_system_reminder injection,
  CacheUsage aggregation across turns.
- 3 — OnMessage: noop, chain fan-out, panic in inner collector
  is caught.
- 5 — fork_handle: take_completion + drop semantics, final-text
  extraction edge cases.
- 9 — fork_subagent end-to-end: Sync inline, ForkAndForget
  returns immediately, timeout cancels loop, abort propagates,
  skip_transcript, on_message, overrides, external_abort.

**Follow-ups** (recorded in `proyecto/FOLLOWUPS.md` if needed):
- 80.19.b: refactor `crates/core/src/agent/delegation_tool.rs`
  to consume `nexo_fork::DefaultForkSubagent` with
  `DelegateMode::Sync` so the existing sync delegation path
  reuses the new isolation contract.
- 80.10 follow-up: actual `agent_handles` row write when
  `skip_transcript: false` — `DefaultForkSubagent` accepts
  a registry but defers the row shape to 80.10's SessionKind.

#### 80.20 — auto-mem `can_use_tool` whitelist for forked dream   ✅

Shipped — `crates/fork/src/auto_mem_filter.rs` (~330 LOC + 24 unit
tests) + `crates/driver-permission/src/bash_destructive.rs::is_read_only`
(~120 LOC + 19 unit tests). Verbatim port of leak
`extractMemories.ts:165-222` `createAutoMemCanUseTool`.

**Public surface**:
- `AutoMemFilter::new(memory_dir)` returns `Result<Self, AutoMemFilterError>`.
  Canonicalize at construction (single syscall, óptimo); fail-fast on
  missing dir.
- `impl ToolFilter for AutoMemFilter` — allow-list per leak.
- `tool_names` module — single source of truth for canonical nexo
  tool name strings (`FileRead`, `FileEdit`, etc.).
- `nexo_driver_permission::bash_destructive::is_read_only` —
  composes Phase 77.8 (`check_destructive_command`) + Phase 77.9
  (`check_sed_in_place`) + redirect/subshell/heredoc detection +
  positive whitelist of ~45 read-only utilities.

**Three pillars applied** (per memory `feedback_provider_agnostic.md`):

- **Robusto**: 15 risks enumerated; 43 unit tests (15 risks × ≥1 test);
  symlink defense via canonical resolve + `starts_with`; path traversal
  caught by canonical resolve; conservative defaults (unknown command
  → deny, missing dir → fail-fast, missing `file_path` → deny,
  non-string args → deny); 4 layers of defense (whitelist + bash
  classifier composition + path canonicalize + post-fork audit in 80.1).
- **Óptimo**: `&'static [&str]` whitelist (zero alloc per call), single
  canonicalize at construction, single source of truth for tool names
  in `tool_names` module, reuses Phase 77.8/77.9 classifiers (no logic
  duplication), `&'static MUTATING_MARKERS` substring scan O(n*m) with
  small constants.
- **Transversal**: operates on tool name + JSON args, no `LlmClient`
  coupling; works under Anthropic/OpenAI/MiniMax/Gemini/DeepSeek; 3
  explicit provider-shape tests (`flat_args_anthropic_shape`,
  `extra_metadata_keys_ignored`, `nested_args_unsupported_explicit`)
  document and verify the flat-args contract.

**Decisions taken**:

- Whitelist intentionally **excludes** `tee`/`awk`/`perl`/`python`/
  `node`/`ruby` even though some uses are harmless — they can shell
  out via `system(...)`. Conservative deny over false-allow.
- `cmake` and other build tools are **not** in the whitelist; the
  redirect-to-/dev/null pass does not flip the verdict.
- Network egress (`curl`/`wget`/`scp`/`rsync`) is conservative-deny
  even for GETs — the auto-dream context is editing memory files,
  not browsing.
- Path traversal `..` defended by canonical resolve before
  `starts_with` (post-resolution comparison, not pre).
- Symlink swap defended by canonicalize-at-construction (resolve
  once) + canonicalize-per-call (resolve every `file_path`).
- Filter expects flat top-level args; nested envelopes are denied
  explicitly (the contract surfaces missing unwraps in provider
  clients immediately).

**Files shipped**:
- `crates/fork/src/auto_mem_filter.rs` (NEW, 24 unit tests)
- `crates/fork/src/lib.rs` re-exports `AutoMemFilter`,
  `AutoMemFilterError`, `tool_names`
- `crates/fork/Cargo.toml` adds `nexo-driver-permission` dep +
  `tempfile` dev-dep
- `crates/driver-permission/src/bash_destructive.rs::is_read_only`
  + `MUTATING_MARKERS` + `READ_ONLY_COMMANDS` + 19 unit tests
- `docs/src/architecture/fork-subagent.md` adds "AutoMemFilter
  (Phase 80.20)" section

**Tests** (43 new — 67 cumulative across crates/fork now):
- 19 — `is_read_only`: simple cmds (ls, grep), pipes, redirects (file
  vs /dev/null), destructive (rm), sed-in-place, subshells, heredoc,
  process-substitution, compound `&&`/`||`/`;`, env-var prefix,
  empty/whitespace, unknown command, sudo/su, curl/wget,
  tee/awk-removed, idempotent.
- 24 — `AutoMemFilter`: construction (existing dir, missing dir),
  allow paths (REPL, FileRead/Grep/Glob, Bash read-only), deny paths
  (Bash destructive, Bash subshell, Bash missing arg, unknown tool),
  FileEdit/FileWrite (existing inside, new file inside, outside,
  relative path, traversal escape, symlink escape, missing file_path,
  non-string file_path, parent missing), denial messages (Bash, Edit,
  generic), provider-shape transversality (3 tests).

#### 80.21 — docs + admin-ui sync   ✅ MVP

**Shipped (public docs sweep)**: 4 nuevas páginas en `docs/src/`
+ 1 sección reorganizada en `SUMMARY.md`:

- `docs/src/agents/assistant-mode.md` (~250 LOC) — concept,
  config quickstart, what changes when on (proactive addendum,
  boot-immutable flag, pairing with auto_approve, always-on
  lifecycle hooks), reading the flag from code, status table
  per Phase 80 sub-phase, cross-refs.
- `docs/src/agents/auto-approve.md` (~280 LOC) — what
  auto-approves (read-only / Bash conditional / scoped writes /
  notifications / coordination), what ALWAYS asks (ConfigTool /
  REPL / remote_trigger / schedule_cron / unknown),
  layered-gates ASCII diagram, YAML config, deferred
  follow-ups (80.17.b.b/c/c), defense-in-depth.
- `docs/src/agents/away-summary.md` (~180 LOC) — config table,
  output shape with truncation, wiring snippet using
  `try_compose_away_digest`, atomic-update pattern, defensive
  edge cases, deferred follow-ups (80.14.b LLM-summarised /
  80.14.c last_seen pairing-store / 80.14.d per-channel).
- `docs/src/agents/multi-agent-coordination.md` (~250 LOC) —
  subject contract `agent.inbox.<goal_id>`, `InboxMessage`
  shape, `list_peers` shape, `send_to_peer` 6 validation gates,
  per-goal fan-out, receive side router + buffer-on-demand
  semantics + 64-msg cap + render shape, wiring snippet,
  defense-in-depth, deferred follow-ups.
- `docs/src/cli/agent-bg.md` (~280 LOC) — `SessionKind` table,
  `agent run [--bg]`, `agent ps`, `agent discover`, `agent
  attach`, 3-tier DB path resolution, kind-aware reattach,
  deferred 80.10.b-g + 80.16.b/c.

`docs/src/SUMMARY.md` reorganizado con nueva sección **Assistant
mode** que agrupa los 4 conceptos + entry de `agent-bg.md` bajo
**CLI**. `mdbook build docs` smoke verde — sin broken links,
todos los cross-refs resuelven.

**Shipped (admin-ui PHASES tech-debt sweep)**: 6 nuevas entradas
en `admin-ui/PHASES.md::Tech-debt registry` — una por feature
operator-visible:

1. **Assistant mode (Phase 80.15)** — Phase A3 toggle +
   addendum textarea + initial_team multi-select + boot-
   immutable "restart required" banner + Phase A4 active-badge.
2. **Auto-approve dial (Phase 80.17)** — Phase A3 toggle +
   workspace-path display + curated-tools preview + Phase A9
   audit log + setup-doctor banner.
3. **Background sessions (Phase 80.10 + 80.16)** — Phase A4
   dashboard tab con SessionKind chips + spawn-BG modal +
   discover-detached pane + per-row drill-in con live-stream
   placeholder.
4. **AWAY_SUMMARY digest (Phase 80.14)** — Phase A3 config
   block + Phase A9 last-digest viewer + per-channel rendering
   preview.
5. **Multi-agent inbox (Phase 80.11 + 80.11.b)** — Phase A8
   delegation visualiser inbox pane con live buffer count +
   per-message preview + drain button + tool registry status +
   API reference for subject contract.
6. **AutoDream cluster (Phase 80.1)** — Phase A7 memory
   inspector AutoDream pane con status badge + audit table tail
   + force-run button gated por `NEXO_DREAM_NOW_ENABLED` +
   capabilities tab row + kill button.

**Three-pillar audit**: robusto (cross-refs entre páginas; status
tables per sub-phase para que operator vea qué está MVP vs
deferred; defensive language sobre "operator hookup pending"
donde aplica), óptimo (mdbook re-uses existing infrastructure;
admin-ui tech-debt list es one-liner per feature, scope-bound),
transversal (cero menciones a LLM-provider en docs nuevos —
provider-agnostic posture; admin-ui entries describen knobs no
prompts).

Memory rule compliance: cero refs a `claude-code-leak/` en las
5 nuevas páginas + 6 nuevas entradas tech-debt + SUMMARY.md.
`grep` verificado.

Standard close-out for any phase touching operator surface.

**Done criteria**:
- New page `docs/src/concepts/kairos-mode.md` registered in
  `SUMMARY.md`: explains assistant_mode, brief mode, channels,
  push, github webhooks, away summary, fork-style consolidation
- `docs/src/operations/cron-jitter.md` documents the 6 knobs +
  hot-reload + how to use as incident shed-load
- `admin-ui/PHASES.md` Phase A-N entry: new "Assistant mode"
  panel listing per-binding `assistant_mode`, `brief`,
  `channels.allowed`, `push.provider`, `kairos_remote_control`,
  `cron.enabled`
- `crates/setup/src/capabilities.rs::INVENTORY` registers
  any new env toggles introduced by 80.12 (`GITHUB_WEBHOOK_SECRET`)
  + 80.13 push provider creds + 80.17
  (`NEXO_KAIROS_REMOTE_CONTROL` if exposed as env override)
- `proyecto/FOLLOWUPS.md` cleared of any 80.* deferred items
  (or each item explicitly tracked there)
- `mdbook build docs` passes locally
- CHANGELOG.md entry

---

**Phase 80 effort estimate**: 80.1 (3-4 days, fork +
locking is the hard part), 80.9 (2-3 days, channels gate is
intricate), 80.12 (1.5-2 days, generic webhook receiver —
re-scoped 2026-04-30 from "github webhook + plugin" to
provider-agnostic receiver per Cristian's request, channels
+ pollers cover the rest of the inbound surface), 80.13
(2 days per provider × 3 providers = 6 days, but APN+FCM
are similar so realistic ~3 days), 80.14 (2 days), 80.15 +
80.17 (1 day each), 80.10 (2 days incl. schema migration),
80.19 (3 days), 80.21 (1 day docs). Everything else ≤ 1 day.
Total ~ 23-28 dev-days for full parity (was 25-30, recortado
~1d por re-scope de 80.12). 80.7 (per-cwd lock) flagged as DEFER until Phase 32
multi-host orchestration arrives.

### Phase 84 — Coordinator agent persona + worker continuation   ⭐ PRIORITY ⬜

> **Status (2026-05-01)**: brainstorm + spec + plan for sub-phase
> 84.1 are approved in conversation. Work paused before
> `/forge ejecutar 84.1`. **Next pickup**: re-enter the forge cycle
> at `/forge ejecutar 84.1` — the plan is committed under
> "84.1 plan" in this section's sub-phase block. No code changes
> have landed; `crates/core/src/agent/personas/` does not exist yet.

Phase 77.18 added the `role: coordinator | worker` binding flag and
gated the tool surface (`TeamCreate` / `TeamDelete` /
`SendToPeer` only callable from coordinator bindings). Phase 79.6
shipped `TeamCreate` / `TeamDelete` for parallel worker spawn.
Phase 80.11 shipped `agent.inbox.<goal_id>` peer messaging with
`ListPeers` + `SendToPeer` discovery + send.

Gap: the **coordinator agent itself does not know it is a coordinator**.
Today the role flag only restricts tool availability — it does not
shape the agent's behavior, prompting, or how it interprets worker
output. A coordinator binding loaded with TeamCreate access still
runs the standard agent system prompt and treats worker results as
opaque chat fragments. This phase closes that gap by adding a
purpose-built coordinator persona prompt, a structured wrapper for
worker results, a continuation tool to re-engage finished workers
with their loaded context, and a complementary worker persona.

**Reference (PRIMARY)**: `research/src/agents/` agent loop +
`research/extensions/team/` (OpenClaw team coordination — TS
single-process pattern; nexo's NATS-based peer mesh is a superset).
**Reference (secondary)**:
- `crates/core/src/agent/list_peers_tool.rs` /
  `send_to_peer_tool.rs` (existing peer surface)
- `crates/fork/src/fork_subagent.rs` (forked subagent run +
  return-value contract)
- Phase 77.18 `EffectiveBindingPolicy.role` (PHASES.md §77.18)
- Phase 79.6 `TeamCreate` / `TeamDelete` (PHASES.md §79.6)

**Done criteria** (phase-level — see sub-phases for granular):
- A binding with `role: coordinator` boots with a coordinator-
  specific system prompt block injected ahead of the standard
  persona; `role: worker` injects a worker-specific block; absent
  role behaves exactly as today.
- Worker / forked-subagent results land in the coordinator's
  context wrapped in a structured `<task-notification>` XML envelope
  carrying `task_id`, `status`, `summary`, optional `result`,
  optional `usage`. The coordinator's prompt teaches it to
  distinguish notifications from real user messages.
- Coordinator can call a new `SendMessageToWorker` (or equivalently
  named) LLM tool to continue a previously-finished worker with its
  loaded context, distinct from `SendToPeer` (which targets a
  separate live peer agent).
- 1 coordinator-side e2e test: spawn 2 workers in parallel,
  receive 2 `<task-notification>` envelopes in order, continue one
  worker via the new tool, verify worker resumed with prior
  context.
- Docs (`docs/src/agents/coordinator-mode.md`) explain the role
  semantics, the notification envelope, and the continue-vs-spawn
  decision matrix. `mdbook build docs` clean.
- `admin-ui/PHASES.md` adds an "Agent role" panel checkbox showing
  per-binding role + whether the coordinator persona is active.

#### 84.1 — Coordinator persona system prompt module   ⬜

New crate-level module (likely `crates/core/src/agent/personas/coordinator.rs`)
that exposes a `coordinator_system_prompt(ctx: CoordinatorPromptCtx) -> String`
builder. The builder produces a single block injected ahead of the
agent's existing system prompt only when
`EffectiveBindingPolicy.role == Coordinator`.

**Content (sections)**:
1. Role declaration — "you are a coordinator; your job is to direct
   workers, synthesize results, communicate with the user".
2. Tool list — current binding's allowed tools focused on the
   coordinator surface (`TeamCreate`, `SendToPeer`,
   `SendMessageToWorker` from 84.3, `TaskStop`, peer discovery).
3. **Continue-vs-spawn matrix** — table guiding when to reuse a
   finished worker vs spawn fresh, keyed on context overlap.
4. **Synthesis discipline** — coordinator must read worker findings
   and craft specific implementation specs with file paths + line
   numbers; explicit anti-pattern: "based on your findings, fix
   the bug" (delegates understanding).
5. **Verification rigor** — define what real verification looks
   like (run tests with feature enabled, investigate failures,
   skeptical reading).
6. **Parallelism guidance** — independent work fans out; concurrent
   tool calls in one assistant message.

**Done criteria**:
- `coordinator_system_prompt` builder + 4 unit tests covering each
  section's presence/absence under different `CoordinatorPromptCtx`
  inputs (empty tools, all tools, scratchpad enabled/disabled,
  custom workers list).
- Per-binding boot wire (`src/main.rs::Mode::Run`) reads
  `effective_policy.role` and prepends the coordinator prompt only
  for `Coordinator` bindings; `Worker` and absent role stay on
  today's prompt path.
- Smoke test: load a YAML fixture with `role: coordinator`, run a
  one-turn goal, assert the coordinator block appears in the
  rendered system prompt.

#### 84.2 — `<task-notification>` envelope for worker results   ⬜

When a forked subagent (`nexo-fork`) or a TeamCreate worker
completes, the coordinator receives the result. Today the result is
either appended as plain text or returned via the tool-call
response path. Phase 84.2 standardizes a single XML envelope that
the coordinator's parser can match deterministically.

**Schema** (single line per attribute, content as inner text):

```xml
<task-notification>
<task-id>{worker_goal_id}</task-id>
<status>completed|failed|killed|timeout</status>
<summary>{one-line outcome}</summary>
<result>{worker's final assistant text — optional}</result>
<usage>
  <total_tokens>{N}</total_tokens>
  <tool_uses>{N}</tool_uses>
  <duration_ms>{N}</duration_ms>
</usage>
</task-notification>
```

**Done criteria**:
- `nexo-driver-types` (or `nexo-fork`) gains a `TaskNotification`
  struct + `to_xml()` renderer + serde round-trip test. Result
  field skipped when worker produced no final text.
- The fork-pass + TeamCreate completion paths render via this
  helper instead of writing free-form text.
- The coordinator persona prompt (84.1) cites the envelope and
  instructs the agent never to `<thank>` or `<acknowledge>`
  notification blocks.
- 3 unit tests: completed-with-result / failed-no-result /
  killed-mid-run — each round-trips and renders without breaking
  XML escaping (`<`, `>`, `&` in body / summary).
- Backwards-compat: legacy callers that read the raw text get the
  rendered envelope; if `task-notification` parsing fails, the
  block is treated as plain text (no panic, no drop).

#### 84.3 — `SendMessageToWorker` continuation LLM tool   ⬜

The coordinator needs to re-engage a finished worker with its
loaded context (Section 5 of the coordinator persona). Today the
choice is binary: spawn a fresh `TeamCreate` worker (loses the
research context) or `SendToPeer` to a live peer (different
semantics — peer is at-rest goal, not a forked subagent that
already returned). Phase 84.3 fills the gap.

**Tool surface**:

```rust
SendMessageToWorker {
    worker_id: String,   // task_id from the prior <task-notification>
    message: String,     // synthesized continuation spec
}
```

Resumes the worker's session with one new user turn appended
holding `message`. Reuses `nexo-fork` session-resume hooks (Phase
67/68 binding store) so the worker re-enters its own context
window, sees its prior tool calls + observations, then acts on the
new spec.

**Done criteria**:
- New tool registered for `role: coordinator` bindings only;
  `EffectiveBindingPolicy` allowlist updated.
- 4 unit tests: success continuation / unknown worker_id (404-style
  error JSON) / worker still running (refuse — distinct from
  `SendToPeer` semantics) / cross-binding worker_id (refuse — only
  workers spawned by this coordinator are reachable).
- 1 integration test: spawn worker → receive notification →
  continue worker → assert resumed session sees prior tool calls
  in transcript.
- Coordinator persona prompt (84.1) gains a worked example showing
  continue-vs-spawn decision applied to a concrete scenario.

#### 84.4 — Worker persona system prompt module   ⬜

Complement to 84.1. Bindings with `role: worker` get a workerspecific
block instead of the coordinator block.

**Content**:
1. Role declaration — "you execute self-contained tasks dispatched
   by a coordinator; do not initiate user-facing dialogue".
2. Output discipline — terse final answers focused on the spec's
   done criteria (file path + commit hash for implementation;
   findings list for research).
3. Self-verification — run typechecks / tests before reporting
   `done`; surface failures with the actual error, not a paraphrase.
4. Tool surface — limited toolkit per `EffectiveBindingPolicy`
   (no `TeamCreate`, no `SendToPeer`, no `SendMessageToWorker`).

**Done criteria**:
- `worker_system_prompt(ctx)` builder + 3 unit tests.
- Boot wire injects worker block when `role: worker`.
- Smoke test parallel to 84.1's: YAML fixture with
  `role: worker`, one-turn goal, worker block appears.

#### 84.5 — Docs + admin-ui sync   ⬜

- `docs/src/agents/coordinator-mode.md` new page covering:
  role flag semantics, persona prompt sections, notification
  envelope schema, `SendMessageToWorker` tool, continue-vs-spawn
  decision matrix.
- `docs/src/agents/worker-mode.md` complementary page.
- Cross-link from existing `docs/src/agents/peer-messaging.md`
  (Phase 80.11) so operators see the full coordinator stack.
- `admin-ui/PHASES.md` "Agent role" panel: per-binding role view +
  active persona indicator.
- `mdbook build docs` clean.
- CHANGELOG `[Unreleased] / Added` entries for 84.1 → 84.4 in the
  same commit that ships 84.5.

**Phase 84 effort estimate**: 84.1 (1.5 days, prompt design + 4
tests + boot wire), 84.2 (1 day, struct + renderer + 3 tests +
fork/team site updates), 84.3 (2 days, tool wiring + session-resume
re-use + 4 unit tests + integration test), 84.4 (1 day, mirror of
84.1 with simpler scope), 84.5 (0.5 day docs). Total ~6 dev-days.
Critical path: 84.1 → 84.2 → 84.3 (84.4 + 84.5 can run in
parallel after 84.1).

### Phase 85 — Compaction hardening: reactive recovery + cache-aware micro-compact   ⬜

Today nexo's compaction subsystem is **proactive only**:
`DefaultCompactPolicy::classify()` (in `crates/driver-types/src/compact_policy.rs:94-156`)
fires before the model rejects the request, using token-pressure +
age + auto-config thresholds. Two well-known gaps from prior-art
review:

1. **No 413 fallback** — when the proactive estimator under-counts
   (e.g. a tool result balloons unexpectedly past `max_tokens`), the
   provider returns `413 prompt_too_long` and the turn dies.
   `crates/llm/src/<provider>` does not currently catch + recover.
2. **Cache-naïve compaction** — when `CompactSummary` rewrites the
   conversation head, the prompt-cache breakpoints recomputed by
   the provider miss; subsequent turns pay full prompt-cache cost.
   For multi-tenant SaaS (Phase 82) this dominates token spend.

Phase 85 addresses both as separate, independently-shippable
sub-phases so 85.1 (defensive, ~1 day) can land before 85.2
(broader, multi-day refactor).

**What will make this better than the reference implementation we
mined**:

- The reference treats reactive vs proactive as **mutually
  exclusive** (a feature flag suppresses proactive entirely when
  reactive mode is on). Nexo will keep proactive as the primary
  guard and add reactive **only as a last-resort safety net** —
  proactive catches the 95% case cheaply, reactive only fires when
  the estimator was genuinely wrong. Strictly better: zero
  added per-turn cost when proactive works.
- The reference's micro-compact pins `CacheEditsBlock` across turns
  in module-level state — non-deterministic across crashes.
  Nexo will surface the cache-pin set as part of `CompactSummary`
  itself (already serde-persisted via `CompactSummaryStore`) so a
  daemon restart resumes with the same cache-edit shape — no
  re-warm-up cost.

#### 85.1 — Reactive 413 recovery   ⬜

Catch `413 prompt_too_long` from the provider, force one compact
pass, retry the turn once. Distinct from proactive: no
suppression — proactive stays on as primary, this is the safety
net.

**Reference shape (file:line — search the local
research/leak tree)**:
- `services/compact/reactiveCompact.ts` — the reactive entrypoint
  module (full reactive flow lives here)
- `services/compact/autoCompact.ts:201-223` — the suppression block
  showing how the reference flips proactive off when reactive is
  on. Nexo deliberately does NOT do this — see header above.
- `query.ts:15` — wrapper `require()` gate. Useful only for naming
  the boundary fn; nexo wires it inside the LLM client retry path
  instead.
- `components/TokenWarning.tsx:130-134` — UI indicator surface
  area. Not applicable (we're a daemon, no TUI).

**Done criteria**:
- New retry classification `ReplayDecision::CompactAndRetry` in
  `crates/driver-types/src/replay_policy.rs` (or extend existing
  `FreshSessionRetry`).
- Provider clients (`crates/llm/src/anthropic`, `minimax`, future)
  intercept HTTP 413 (or the SDK's typed equivalent), return a
  typed `LlmError::PromptTooLong { tokens_used, tokens_limit }`
  instead of generic 4xx → `Other`. Today they likely surface as
  generic provider error.
- Driver-loop replay policy classifies `PromptTooLong` →
  `CompactAndRetry`. The orchestrator forces a `CompactPolicy`
  pass (`Trigger::Reactive413`) without consulting the proactive
  estimate, then retries the same turn ID once.
- New `BudgetGuards` axis `max_consecutive_413: u32` (default 2)
  prevents infinite loop when compact still doesn't fit. Exhaustion
  → goal aborts with `BudgetAxis::Consecutive413`.
- 4 unit tests in `crates/driver-types/src/replay_policy.rs`:
  classification of `PromptTooLong`, budget axis exhaustion, retry
  count reset on success, no double-compact when consecutive < 2.
- 1 integration test under `crates/driver-loop/tests/`: mock
  provider returns 413 once, then succeeds; orchestrator records
  one compact + one successful turn; transcript shows the compact
  marker between attempts.
- Telemetry: span `driver_loop.compact.reactive` + counter
  `compact_reactive_total{outcome=succeeded|exhausted}` for
  Prometheus once 28.x ships.

#### 85.2 — Cache-aware micro-compaction   ⬜

Today `CompactSummary` rewrites the conversation head wholesale.
The prompt cache (Anthropic 5-min TTL, MiniMax similar) keys on
exact prefix bytes — any rewrite invalidates downstream
breakpoints. For autonomous loops doing 50+ turns, the lost cache
hit ratio inflates token spend by 30-50% on long-context models.

**Reference shape (file:line)**:
- `services/compact/microCompact.ts:52-81` — module-level cache
  state (`cachedMCModule`, `cachedMCState`, `pendingCacheEdits`)
  + lazy init via `ensureCachedMCState()`. Useful for naming +
  understanding the variable shape; nexo's persistence model
  differs (see header — we serde-persist, they do module-level).
- `services/compact/microCompact.ts:36` — `TIME_BASED_MC_CLEARED_MESSAGE`
  marker string for tool-result truncation. Useful as the pattern
  reference; nexo will define its own marker constant.
- `services/compact/microCompact.ts:276` — consumption point in
  the API request prep. Tells you the integration boundary.
- `query.ts:423` — feature gate. Not needed; nexo will gate via
  `CompactPolicy::micro_enabled: bool` config field.

**Done criteria** (hardness ranked low → high):
1. New `MicroCompactPolicy` trait in `crates/driver-types/src/compact_policy.rs`,
   default impl `DefaultMicroCompactPolicy` triggers when
   `tool_result_bytes > 8KB` AND `turn_index < cache_breakpoint`.
2. `CompactSummary` extended with `cache_pin_keys: Vec<String>` +
   `truncated_tool_results: Vec<TruncatedToolResult { call_id,
   original_byte_size, marker_inserted_at_turn }>`. Backwards-
   compat via `#[serde(default)]`.
3. Driver-loop orchestrator consults `MicroCompactPolicy` per-turn
   (cheap O(1) check) before assembling the request body. When
   triggered, replaces the full tool result with the marker
   constant + records the truncation in `CompactSummary`.
4. Provider clients build the request honoring `cache_pin_keys` —
   they prepend cache_control breakpoints at the pinned positions,
   so the provider keeps the cached prefix intact across compact
   passes.
5. `CompactSummaryStore` (already persisted) round-trips the new
   fields — 1 migration test in `crates/driver-loop/tests/`
   confirming a 84.x summary deserializes cleanly with empty
   pin_keys + truncations.
6. 6 unit tests + 1 integration test: small tool result (no
   truncate), oversized tool result (truncate + marker), cache pin
   survives compact, restart re-loads pin set, two consecutive
   compacts do not double-mark the same call_id, marker is
   provider-agnostic (Anthropic + MiniMax fixtures).
7. Telemetry: `compact_micro_truncated_bytes_total` counter +
   `compact_micro_cache_hit_ratio` gauge.

**Phase 85 effort estimate**: 85.1 (~1 day, mostly typed-error
plumbing + 1 retry path), 85.2 (~3-4 days, cache-pin shape across
2+ providers + serde migration). Total ~4-5 dev-days. 85.1 ships
first as a defensive standalone; 85.2 is bigger but uncorrelated.

**Out of scope (deferred)** — three more reference-tree compaction
features evaluated and explicitly skipped:

- Granular per-message context-collapse (`services/contextCollapse/`).
  Async LLM call per collapsed message — latency + complexity not
  justified until turn-mortality metrics show > 5% goal failure
  from context overflow.
- User-driven history snipping (`tools.ts:123` flag + picker UI in
  `components/PromptInput/PromptInput.tsx:1721-1727,2144-2156`).
  Daemon has no TUI; admin-ui equivalent is low value (better to
  kill + relaunch a stuck goal).
- Pending-compact reminders attachment (`utils/attachments.ts:922,
  3931-3955`). Targets a human user in the loop. Nexo agents run
  autonomous — operator observability already lives in admin-ui
  metrics.

### Phase 86 — Memory observability + cache debug affordance   ⬜

Two unrelated cache/memory affordances surfaced from prior-art
review. They are bundled here because both are small, both deepen
operator visibility, and both leverage the existing Phase 28
metrics infrastructure.

**What will make this better than the reference implementation we
mined**:

- The reference's memory-shape telemetry **phones home to an
  upstream analytics endpoint**. Nexo deliberately routes the same
  signals to **local Prometheus + admin-ui only** — zero outbound
  network calls. Strictly better for a self-hosted framework: the
  operator gets the diagnostic data, no third-party gets it.
- The reference's cache-break command lives in the interactive TUI
  prompt input. Nexo is a daemon — exposing it as a `nexo agent
  debug` CLI subcommand means it's scriptable + audit-loggable
  through the existing CLI surface.

#### 86.1 — Local memory-shape metrics (Prometheus)   ⬜

Emit counters + histograms for memory recall + write activity
to the Phase 28 metrics registry. Operator wires them into Grafana
panels via the existing scrape endpoint. No phone-home.

**Reference shape (file:line — search the local research/leak
tree)**:
- `memdir/findRelevantMemories.ts:66` — feature gate +
  `logMemoryRecallShape(memories[], selected[])` call. The
  denominator (all available) vs. numerator (selected) is the
  selection-rate signal.
- `utils/sessionFileAccessHooks.ts:210` —
  `logMemoryWriteShape(tool_name, input, filePath, scope)` write
  side. The `scope` field maps to nexo's
  `MutationScope::SqliteLongTerm` etc.

**Done criteria**:
- `crates/memory/src/long_term.rs::remember_typed` emits
  `memory_write_total{agent_id, type}` counter where `type` ∈
  `{user, feedback, project, reference}` per the Phase 77.5
  taxonomy.
- `crates/memory/src/long_term.rs::recall*` (every public recall
  fn) emits `memory_recall_total{agent_id, scope}` counter +
  `memory_recall_selected_ratio` gauge (selected / available;
  recorded as ratio not raw counts so cardinality stays bounded).
- `crates/driver-loop/src/extract_memories.rs::store_extracted`
  emits `memory_write_size_bytes` histogram bucketed at 256 / 1k /
  4k / 16k / 64k bytes.
- `crates/memory/src/long_term.rs::recall*` emits
  `memory_age_at_recall_seconds` histogram per recalled memory
  (max age — captures long-tail retrieval).
- All metrics gated by Phase 28's existing
  `NEXO_METRICS_ENABLED` env (no new toggle, no new INVENTORY
  entry).
- 4 unit tests in `crates/memory/src/long_term.rs::tests` covering
  one emit per fn (write, recall hit, recall miss, age histogram
  bucket).
- 1 integration test in `crates/memory/tests/`: write 5 memories
  of mixed types → recall → assert all 4 metric families recorded
  with expected label sets.
- `docs/src/operations/memory-observability.md` page lists the new
  metrics + a sample Grafana panel JSON for "memory health" with
  selection-rate trend, write-volume by type, and age histogram.
- `admin-ui/PHASES.md` "Memory observability" panel checkbox:
  surfaces the same metrics via the admin-ui dashboard backend.

#### 86.2 — `nexo agent debug break-cache` CLI subcommand   ⬜

Operator-facing tool to force a prompt-cache miss on a binding's
next turn. Use case: debugging cache regression in production
when automatic detection (Phase 77.4) reports nothing but an
operator suspects a stale cache hit. Audit-logged.

**Reference shape (file:line)**:
- `context.ts:131` — feature gate + ephemeral `systemPromptInjection`
  string mechanism. The injection-into-system-prompt approach is
  the pattern; the slash-command UX is irrelevant to a daemon.

**Done criteria**:
- New CLI subcommand `nexo agent debug break-cache --binding=<id>`
  in `crates/cli` (or wherever the existing `nexo agent` subcommand
  lives — likely `src/main.rs` `Mode::Agent`).
- Subcommand sets a one-shot `[CACHE_BREAKER: <uuid>]` injection
  on the named binding's next system-prompt assembly. Cleared
  after consumption (idempotent — second call without a turn in
  between overwrites the prior uuid).
- Mechanism: extend `EffectiveBindingPolicy` with
  `pending_cache_breaker: Option<String>` (transient, NOT
  persisted; lives in the runtime cache only). Cleared on
  consumption inside `resolve_prompt`.
- Audit log entry `agent.debug.cache_breaker_set{binding, uuid,
  operator_caller}` emitted on the broker so admin-ui +
  Prometheus see who triggered the break.
- 3 unit tests: subcommand wiring (parses flags), policy carries
  the breaker into `resolve_prompt`, second turn after consumption
  has no breaker.
- 1 integration test under `crates/driver-loop/tests/`: simulate
  the CLI flow → first turn's rendered system prompt contains
  `[CACHE_BREAKER:`, second turn does not.
- Capability inventory: NO new entry needed (operator-only CLI,
  no env toggle, requires server-side admin auth via existing
  `nexo agent` auth gate).

**Phase 86 effort estimate**: 86.1 (~1 day, 4 metric families +
6 tests + docs page), 86.2 (~0.5 day, transient policy field +
1 CLI subcommand + audit log + 4 tests). Total ~1.5 dev-days.
Uncorrelated — either ships first.

### Phases 82 + 83 — Microapp framework

The full planning for the microapp framework — Phase 82
(multi-tenant SaaS extension enablement + control plane
primitives) and Phase 83 (microapp framework foundation,
SDK, templates, reference microapps) — has been **moved to
its own document**: `proyecto/PHASES-microapps.md`.

That file is the **source of truth** for any topic prefixed
`82.x` or `83.x`. When `/forge brainstorm | spec | plan |
ejecutar` is invoked on a microapp-related topic, read
`PHASES-microapps.md` before consulting this legacy file.

Status summary:

| Phase | Name | Sub-phases | Status |
|-------|------|-----------|--------|
| 82 | Multi-tenant SaaS extension enablement + control plane | 12 | 0/12 |
| 83 | Microapp framework foundation | 11 | 0/11 |

Rationale: keeping the microapp framework planning in a
dedicated file avoids polluting this 9 000-line legacy
document, makes `/forge` context smaller, and clarifies
ownership. The legacy file (`proyecto/PHASES.md`) carries
Phases 1-81 only; everything microapp-related lives in
`proyecto/PHASES-microapps.md`.

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
