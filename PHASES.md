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

#### 26.z — `tunnel.url` integration in URL resolver   ⬜

Tracks **PR-3** in `FOLLOWUPS.md`. `nexo pair start` honours only
`--public-url` today; spec'd priority chain places `tunnel.url`
second. Blocked on `nexo-tunnel` exposing a read-only public-URL
accessor (small refactor in tunnel crate).

#### 26.aa — `pair_approve` scope-gated agent tool   ⬜  (security review required)

Tracks **PR-5** in `FOLLOWUPS.md`. Built-in tool that lets agents
approve pending pairings from a trusted channel, scoped via
`EffectiveBindingPolicy::allowed_tools`. Opens prompt-injection
vectors — needs a clear trust model before landing.

#### 26.ab — `config/pairing.yaml` loader   ⬜

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

#### 38.x — Test flakes & real concurrency races   ⬜

Two unrelated flakes that surface under CI parallelism. Bundled
because Phase 38 (chaos / property tests) is where this kind of
work lives.

- **38.x.1** `nexo_llm::telemetry::tests::render_empty_series_when_no_samples`
  flakes under `cargo test --workspace` due to global `LazyLock`
  state shared across parallel test binaries. Fix: `serial_test`
  guard, or scope the global behind a `cfg(test)`-only registry
  reset.
- **38.x.2** `nexo_core::agent::transcripts::tests::concurrent_first_appends_only_write_one_header`
  is `#[ignore]`'d today — it surfaces a real race in
  `TranscriptWriter::append_entry`: the writer that wins the
  `create_new` open isn't guaranteed to flush the header before
  other writers open in `append` mode. Real fix needs a per-session
  in-process Mutex (`DashMap<(root, session_id),
  Arc<tokio::Mutex<()>>>`) around the header-creation block. The
  bug is dormant in production today because plugins serialise
  inbound events per session at the broker, but it's a foot-gun for
  any future caller that fans out concurrent appends.

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

#### 27.1 — `cargo dist` baseline

- Add `dist-workspace.toml` describing supported targets:
  `linux-x86_64-musl`, `linux-aarch64-musl`, `macos-x86_64`,
  `macos-aarch64`, `windows-x86_64`.
- `cargo dist init` integrated; `dist build` produces tarballs
  locally as a smoke gate.
- `Cargo.toml` package metadata (license, repo, description)
  audited so crates.io / package indexes get clean info.
- `CHANGELOG.md` enforced via `release-please` or `git-cliff` —
  every PR adds a `## Unreleased` line, release flips to
  versioned section.

Done when `cargo dist build --tag v0.1.0` produces the full
matrix of binaries on a developer laptop without errors.

#### 27.2 — GitHub Actions release workflow

- `.github/workflows/release.yml` triggers on `v*` tags.
- Matrix strategy spawns one runner per target.
- Caches: `cargo` registry, `target/`, `sccache` if available.
- Artifacts uploaded to the GH release page.
- Release notes auto-built from `CHANGELOG.md` between tags.
- Concurrency guard so two tag pushes don't double-publish.

Done when pushing `git tag v0.1.0 && git push --tags` produces
a GH release with all 5 platform tarballs + sha256 sums within
20 min.

#### 27.3 — Cosign / sigstore signing

- Sign every tarball with sigstore keyless (OIDC via GH
  Actions identity) to a Rekor transparency log.
- Publish `*.sig` and `*.bundle` next to each tarball.
- A `verify.sh` snippet in `docs/install/verify.md` showing how
  an operator verifies a download.
- Optional: a long-lived cosign key for the homebrew tap so
  `brew` validates without OIDC chain.

Done when an operator can `cosign verify-blob --bundle …
nexo-rs.tar.gz` on a downloaded artifact and get green.

#### 27.4 — Debian + RPM packages   🔄

Build recipes + systemd unit + maintainer scripts shipped; signed
apt / yum repo deferred (block on Phase 27.3 GPG/cosign infra).

Shipped:
- `packaging/debian/build.sh` — produces
  `dist/nexo-rs_<version>_<arch>.deb` for amd64 (native) and arm64
  (cross via `cargo-zigbuild`). Reads version + description from
  `Cargo.toml`. Pre-install creates the `nexo` system user;
  post-install owns `/var/lib/nexo-rs/`, `/var/log/nexo-rs/`,
  `/etc/nexo-rs/` mode 0750; pre-removal stops the unit;
  post-purge wipes state + drops the user.
- `packaging/debian/nexo-rs.service` — systemd unit with hardening
  defaults: `NoNewPrivileges`, `ProtectSystem=strict`,
  `ProtectHome`, `PrivateTmp`, `PrivateDevices`,
  `ProtectKernel*`, `ProtectControlGroups`, `RestrictNamespaces`,
  `LockPersonality`, `RestrictRealtime`, `RestrictSUIDSGID`,
  `RemoveIPC`, `ReadWritePaths` scoped to state + log dirs,
  `LimitNOFILE=65536`, `TasksMax=4096`. `Restart=on-failure`
  with backoff. 30s SIGTERM grace.
- `packaging/debian/README.md` — local build paths, install
  command, what the postinst sets up, hardening notes, removal
  semantics, deferred apt-repo publish plan.
- `packaging/rpm/nexo-rs.spec` — RPM spec for Fedora / RHEL /
  openSUSE. Same systemd unit reused as `Source1`. `%pre` creates
  the `nexo` user, `%post` chowns + prints next steps,
  `%preun`/`%postun` use the systemd-rpm-macros for proper
  service handling, `%postun` on full removal wipes state.
- `packaging/rpm/build.sh` — produces `dist/nexo-rs-<version>-1.<dist>.<arch>.rpm`
  for x86_64 (native) and aarch64 (cross). Stages a source tarball,
  invokes `rpmbuild` with the workspace version injected.

Deferred:
- Apt repo at `https://lordmacu.github.io/nexo-rs/apt/` with a
  signed `Release` file + GPG key — needs the key infra from
  Phase 27.3.
- Yum / dnf repo equivalent at `.../yum/` with `RPM-GPG-KEY-nexo`.
- GitHub Pages publish job that mirrors `dist/*.deb` /
  `dist/*.rpm` into the repo layout — needs Phase 27.2 release
  workflow.
- Auto-test that the deb actually installs cleanly on a fresh
  Debian 12 / Ubuntu 24.04 container (CI matrix step).

Done when (revised): an Ubuntu user adds the apt repo, runs
`apt install nexo-rs`, and ends up with a daemon under systemd
that came from a signed package. Recipe + unit + scripts done
now; signed-repo half blocks on 27.2 + 27.3.

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

#### 27.9 — SBOM + reproducibility

- SBOM (CycloneDX or SPDX) emitted per artifact via
  `cargo cyclonedx` or `syft`.
- Attached to each GH release as `sbom-*.json`.
- Reproducible build attestation: same git sha + same toolchain
  → bit-identical artifacts. Documented test reproduction steps.
- SLSA Level 2 attestation (provenance via GitHub Actions OIDC
  + sigstore).

Done when `slsa-verifier verify-artifact …` against any
release passes and a third party can rebuild the artifact and
get the same sha256.

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

### Phase 50 — Privacy toolkit (GDPR-ish)

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

### Phase 51 — Eval harness + prompt versioning

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
