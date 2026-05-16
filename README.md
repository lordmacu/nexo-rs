# nexo-rs — Rust multi-agent LLM framework (OpenClaw alternative)

> **The Rust alternative to [OpenClaw](https://github.com/openclaw/openclaw).**
> Multi-agent LLM gateway for WhatsApp, Telegram, Gmail and the browser —
> single static binary, event-driven (NATS *or* a built-in local broker —
> no external infra), fault-tolerant, MCP-native. Boots with zero config.

[![docs](https://img.shields.io/badge/📘%20docs-lordmacu.github.io%2Fnexo--rs-blue?style=for-the-badge)](https://lordmacu.github.io/nexo-rs/)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green?style=for-the-badge)](#license)
[![rust](https://img.shields.io/badge/built%20with-Rust-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![openclaw alternative](https://img.shields.io/badge/OpenClaw-alternative-purple?style=for-the-badge)](#inspired-by-openclaw-supercharged-by-rust)

**📘 Full documentation: <https://lordmacu.github.io/nexo-rs/>**

[Quick start](https://lordmacu.github.io/nexo-rs/docs/getting-started/quickstart.html) ·
[Architecture](https://lordmacu.github.io/nexo-rs/docs/architecture/overview.html) ·
[Recipes](https://lordmacu.github.io/nexo-rs/docs/recipes/index.html) ·
[vs OpenClaw](https://lordmacu.github.io/nexo-rs/docs/architecture/vs-openclaw.html) ·
[Contributing](https://lordmacu.github.io/nexo-rs/docs/contributing.html) ·
[Releases](https://github.com/lordmacu/nexo-rs/releases) ·
[License](#license)

**nexo-rs** is a **Rust multi-agent LLM framework** — an
**OpenClaw alternative in Rust** — for building **WhatsApp bots**,
**Telegram bots**, **Gmail pollers** and **browser agents** behind a
single binary. Event-driven over **NATS** *or* a built-in **local
broker** (stdio-bridge for subprocess plugins — no NATS server,
no external infra), with per-agent tool sandboxes, **MCP** client +
server, **Claude / Anthropic / MiniMax / OpenAI-compatible / Gemini /
DeepSeek** providers, durable workflows (TaskFlow), and drop-in
configuration for private vs. public agents.

One process, many agents, many channels. Kate handles your personal
Telegram; Ana works the WhatsApp sales line; a cron-style poller sweeps
Gmail for leads — all sharing one broker, one tool registry, one
memory layer.

Install the pre-built binary (`curl … install.sh | bash` on Linux/macOS,
`cargo install nexo-rs` on Windows, or a
`.deb` / `.rpm` / Termux `.deb` from
[GitHub Releases](https://github.com/lordmacu/nexo-rs/releases/latest)),
then `nexo` — the daemon boots against documented defaults with no
config dir. Add a channel plugin with `nexo plugin install
<owner>/<repo>`, a persona with `nexo persona install <owner>/<repo>`,
or scaffold 19 commented sample YAMLs with `nexo init`.

**Keywords:** OpenClaw alternative · Rust agent framework · multi-agent
LLM · WhatsApp LLM bot · Telegram AI agent · Gmail agent · MCP server
Rust · NATS agent gateway · Claude Code subscription · Anthropic OAuth
PKCE · agent framework Termux · self-hosted AI agents.

---

## Install

Every channel produces the same `nexo` binary — the differences are
how it lands on your machine. The one-liner needs no Rust toolchain.

| Method | Command | Notes |
|--------|---------|-------|
| **Pre-built binary (Linux / macOS)** | `curl -fsSL https://lordmacu.github.io/nexo-rs/install.sh \| bash` | Linux x86_64 / aarch64 (static musl), macOS Intel / Apple Silicon. Detects OS+arch, verifies sha256, drops `nexo` on PATH, then installs the bundled plugins + a persona. Falls back to `cargo install nexo-rs`. Works on Windows too when run from Git Bash. |
<!-- Windows PowerShell installer temporarily hidden while the
     script catches up with the multi-binary plugin layout
     (Phase 81.20.x Stage 7 Phase 2). Use `cargo install nexo-rs`
     below or grab the zip from GitHub Releases.
| **Pre-built binary (Windows, PowerShell)** | `irm https://lordmacu.github.io/nexo-rs/install.ps1 \| iex` | Downloads `nexo-rs-x86_64-pc-windows-msvc.zip`, verifies sha256, adds `nexo.exe` to your user PATH, then installs the bundled plugins + a persona. |
-->
| **Windows (zip from Releases)** | Download `nexo-rs-x86_64-pc-windows-msvc.zip` from [Releases](https://github.com/lordmacu/nexo-rs/releases/latest) and extract `nexo.exe` to a folder on PATH. | Verify the bundled `*.sha256` manually before extracting. Or just use the `cargo install` row below. |
| **crates.io** | `cargo install nexo-rs` | Needs a Rust toolchain (1.80+). Compiles the daemon from the published workspace. Works on every platform Rust supports. |
| **Debian / Ubuntu** | `sudo apt install ./nexo-rs_<ver>_amd64.deb` | Download the `.deb` from [Releases](https://github.com/lordmacu/nexo-rs/releases/latest). Ships a systemd unit; postinst seeds `/etc/nexo-rs/`. |
| **Fedora / RHEL / Rocky** | `sudo dnf install ./nexo-rs-<ver>-1.x86_64.rpm` | `.rpm` from [Releases](https://github.com/lordmacu/nexo-rs/releases/latest). Same systemd unit. |
| **Termux (Android, aarch64)** | `pkg install ./nexo-rs_<ver>_aarch64.deb` | aarch64 `.deb` (bionic libc) from [Releases](https://github.com/lordmacu/nexo-rs/releases/latest). No root. |
| **Docker / GHCR** | `docker pull ghcr.io/lordmacu/nexo-rs:latest` | Multi-arch (`amd64` + `arm64`); bundles Chrome, cloudflared, ffmpeg, tesseract, yt-dlp; carries SBOM + SLSA provenance. |
| **Nix flake** | `nix run github:lordmacu/nexo-rs` | Reproducible dev shell + binary. |
| **From source** | `git clone https://github.com/lordmacu/nexo-rs && cd nexo-rs && cargo build --release` | Track `main`. `--profile release-fast` for ~50 % quicker iterative builds. |

Every release artifact (tarball, `.deb`, `.rpm`, `.zip`) ships a
`.sha256` sidecar and a cosign signature. Per-OS prerequisites + the
optional-feature (STT / voice / browser) matrix:
[Platform support](https://lordmacu.github.io/nexo-rs/docs/getting-started/platform-support.html).
After installing, `nexo` boots with zero config — see
[Quick start](#quick-start) below.

---

## Why

Most "agent frameworks" assume a single LLM talking to a single user
through a single UI. Real deployments aren't that shape:

- Several agents with different personas, models, and skills
- Multiple channels (WA + Telegram + mail) feeding the same agents
- Business logic that isn't LLM-driven (scheduled tasks, regex email
  triage, lead notifications) running next to the LLM loop
- Private prompts and pricing tables alongside an open-source core

nexo-rs is opinionated toward that shape.

## Inspired by OpenClaw, supercharged by Rust

nexo-rs stands on the shoulders of [OpenClaw](https://github.com/openclaw/openclaw):
its plugin SDK, its multi-channel gateway shape, and its
"agents-on-real-messengers" thesis are the seed of this project.
OpenClaw proved the model works. nexo-rs takes that blueprint and
rebuilds it on a substrate Node simply cannot match — Rust, tokio,
NATS — so the runtime can do things the original never could:

- **Single static binary, ~90 MB** (the release tarball you download
  is ~15 MB, xz-compressed; `.deb` ~18 MB, `.rpm` ~25 MB). No
  `pnpm install`, no `node_modules`, no runtime VM. Drop the binary
  on a Termux phone, a Raspberry Pi, a Proxmox container — it runs.
- **Memory-safe by construction.** Rust's ownership model refuses
  whole bug classes at compile time: use-after-free, data races,
  null deref. The agent loop is `unsafe`-free.
- **True parallelism.** tokio + per-agent runtimes + share-nothing
  per session. No JS event loop bottleneck — Kate, Ana, the gmail
  poller, the browser CDP session and 30 LLM turns run on real
  threads at once.
- **Fault-tolerant by default.** Every external call sits behind a
  circuit breaker. NATS down? In-process mpsc + on-disk queue + DLQ
  takes over and drains on reconnect. No dropped messages, no
  duplicates.
- **Hot reload without dropping in-flight turns.** `agent reload`
  swaps a `RuntimeSnapshot` via `ArcSwap`; turns mid-flight finish
  on the old config, new turns pick up the new one.
- **Per-agent capability sandbox.** `allowed_tools`,
  `outbound_allowlist`, `skill_overrides`, `accept_delegates_from`
  and per-binding overrides — the LLM literally never sees tools
  it isn't entitled to.
- **Durable workflows (TaskFlow).** `wait` / `finish` / `fail` LLM
  tools with `Timer` / `ExternalEvent` / `Manual` resume — flows
  survive restarts.
- **Both sides of MCP.** Client (stdio + HTTP) **and** agent-as-server
  (`agent mcp-server`).
- **Claude subscription auth.** OAuth PKCE on top of API keys —
  reuse your Claude Code subscription quota.
- **Observability built in.** Prometheus on `:9090`, health on
  `:8080`, admin console on `127.0.0.1:9091`, JSONL transcripts
  with FTS5 + opt-in regex redactor.

### Side-by-side

| Dimension | OpenClaw | nexo-rs |
|-----------|----------|---------|
| Language / runtime | TypeScript on Node 22+ | Rust, no runtime — single binary; pre-built for Linux / macOS / Windows + `.deb` / `.rpm` / Termux |
| Install footprint | `pnpm install` (~42 runtime deps) + Node | one **~90 MB** binary; ~15 MB to download (xz tarball), ~18 MB `.deb`, ~25 MB `.rpm` |
| First boot | wire `.env` + config before it runs | zero-config — `nexo` boots against documented defaults; `nexo init` scaffolds 19 commented YAMLs |
| Broker / process model | single Node process, in-memory | NATS (multi-host) **or** a local stdio bridge (single-host, no external server); subprocess plugins talk to whichever; `nexo set-broker` flips at runtime |
| Fault tolerance | best-effort | broker has disk queue + DLQ + circuit breaker; events survive NATS outages |
| Concurrency | JS event loop | tokio async + per-agent runtimes, share-nothing per session |
| Hot reload | restart | `agent reload` swaps `RuntimeSnapshot` via `ArcSwap` — in-flight turns keep the old config |
| Per-agent capability sandbox | global plugin allowlist | `agents.<id>` has `allowed_tools`, `outbound_allowlist`, `skill_overrides`, `accept_delegates_from`, per-binding overrides |
| Secret ops | env vars | `agents.<id>.credentials` per-channel + 1Password `inject_template` + JSONL audit log + `agent doctor capabilities` inventory |
| Transcripts | JSONL (line-grep) | JSONL **plus** SQLite FTS5 index + opt-in regex redactor (Bearer JWT, sk-…, AKIA…, paths) |
| Durable workflows | n/a | TaskFlow: `wait`/`finish`/`fail` LLM tools with `Timer`/`ExternalEvent`/`Manual` resume + `taskflow.resume` NATS bridge |
| Claude auth | API key only | API key **and** `claude_subscription` OAuth PKCE flow (uses your Claude Code subscription quota) |
| MCP | client only | client (stdio + HTTP) **and** agent-as-MCP-server (`agent mcp-server`) |
| Mobile-friendly | requires Node + npm runtime | runs on Termux without root (no Docker, no Node) |
| Memory safety | runtime errors | Rust ownership model — whole classes of bugs (use-after-free, data races, null deref) refused at compile time |
| Bundled skills/extensions | many in `extensions/` | 20+ skills + 30+ extensions (browser, gmail-poller, 1password, proxmox, ssh-exec, brave-search, wolfram-alpha, …) |
| Plugin SDKs | TypeScript | Rust, Python, TypeScript, PHP — same wire spec; `nexo plugin install <owner>/<repo>` |

**What we did not improve.** OpenClaw has a richer installer flow,
a longer track record, and a TypeScript surface that's familiar to
a wider audience. If your shop is JS-first, picking it up is
faster than learning Rust. nexo-rs trades that approachability for
the operational properties above.

Full side-by-side: [docs → vs-openclaw](https://lordmacu.github.io/nexo-rs/docs/architecture/vs-openclaw.html).

## Status

**v0.1.6 — GA.** Stable [release](https://github.com/lordmacu/nexo-rs/releases/latest)
with pre-built binaries for Linux (x86_64 / aarch64, static musl),
macOS (Intel / Apple Silicon) and Windows, plus `.deb` / `.rpm` /
Termux `.deb` packages, a multi-arch container
(`ghcr.io/lordmacu/nexo-rs`), and crates.io publishing. Used in
production for WhatsApp / Telegram sales bots and personal-assistant
agents.

Shipped, end-to-end:

- Multi-agent runtime — SessionManager, Heartbeat, CircuitBreaker, agent-to-agent routing + delegation
- Broker — NATS (disk queue + DLQ + backpressure + single-instance lock) **or** `local` stdio-bridge; `nexo set-broker` flips at runtime; zero-config boot, `nexo init` scaffolds
- LLM stack — MiniMax M2.5 (primary), Anthropic (API + OAuth PKCE / Claude subscription), OpenAI-compatible, Gemini, DeepSeek; per-provider rate limit + retry
- Channels — WhatsApp (Signal Protocol, QR pairing), Telegram (full bot API), Email (Gmail poller + outbound), Browser (Chrome DevTools Protocol), Google (Gmail / Calendar / Drive / Sheets) — extracted to standalone plugin repos, `nexo plugin install <owner>/<repo>`
- Memory — short-term in-memory, long-term SQLite, vector via sqlite-vec; snapshots
- Extensions — TOML manifest, stdio + NATS runtimes, CLI, 20+ skills, plugin SDKs in Rust / Python / TypeScript / PHP
- MCP — client (stdio + HTTP) **and** agent-as-server (`nexo mcp-server`), hot-reload
- TaskFlow — durable multi-step flows with Timer / ExternalEvent / Manual resume
- Soul — identity, MEMORY.md, dreaming, workspace-git forensics, JSONL transcripts (FTS5 + redactor)
- Personas — out-of-tree agent packs (v2 manifest, GitHub Releases), `nexo persona install <owner>/<repo>`
- Ops — Prometheus metrics, health endpoint, loopback admin console, hot-reload, secrets audit, cosign-signed release artifacts

Release-by-release notes live in [`CHANGELOG.md`](CHANGELOG.md).

## Architecture

```
              ┌──────────────────┐
              │  NATS broker     │◄── fault-tolerant event bus
              │  — or local      │      (disk queue + DLQ when
              │  stdio bridge,   │       offline; local mode pipes
              │  no NATS server  │       broker.publish/event over
              └──┬────────────┬──┘       the daemon's stdio JSON-RPC)
 plugin.inbound.*│            │plugin.outbound.*
                 │            │
    ┌────────────┴──┐       ┌─┴─────────────┐
    │  Channel      │       │  Agent        │
    │  plugins      │       │  runtime      │
    │ (WA, TG, mail)│       │ (Kate, Ana…)  │
    └───────────────┘       └───┬───────────┘
                                │
                  ┌─────────────┼─────────────────────┐
                  │             │                     │
            ┌─────┴──┐    ┌─────┴──────┐      ┌───────┴────────┐
            │ Memory │    │ Extensions │      │  LLM providers │
            │(SQLite │    │ (stdio /   │      │ (Anthropic /   │
            │+ vec)  │    │  NATS /    │      │  MiniMax /     │
            └────────┘    │  MCP)      │      │  OpenAI-compat)│
                          └────────────┘      └────────────────┘
```

Every external call goes through a circuit breaker. NATS offline? The
plugin falls back to a local tokio mpsc + disk queue and drains on
reconnect. No events lost.

## Quick start

```bash
# 1. Install — pre-built binary, no Rust toolchain needed:
curl -fsSL https://lordmacu.github.io/nexo-rs/install.sh | bash
#    OR  cargo install nexo-rs            # from crates.io
#    OR  docker pull ghcr.io/lordmacu/nexo-rs:latest
#    OR  download the .deb / .rpm / Termux .deb from
#        https://github.com/lordmacu/nexo-rs/releases/latest

# 2. Run — Phase 92-95: zero config, no NATS server needed
nexo
```

That's it for a smoke test. The daemon boots with sane
defaults (broker=local, sqlite memory, 0 agents) and serves
admin RPCs + health. From here, three paths to fill it in:

```bash
# A) Scaffold sample YAMLs (heavily commented)
nexo init                       # all 19 templates to ~/.config/nexo/
nexo init --yaml broker,llm     # just the ones you want
vi ~/.config/nexo/llm.yaml      # edit, set ${MINIMAX_API_KEY} env
nexo

# B) Switch broker at runtime (no YAML editing)
sudo systemctl start nats-server
nexo set-broker nats --url nats://localhost:4222

# C) Interactive wizard (classical path)
nexo setup                       # pairs WhatsApp, asks for API key,
                                  # walks Google OAuth for Gmail
```

`nexo setup` is interactive — pairs WhatsApp via QR, asks for
your LLM API key, walks the Google OAuth consent if you want
Gmail. Use it when you want a guided walkthrough; otherwise
edit `nexo init`'s scaffolded YAMLs by hand or call admin RPCs
from the operator UI.

> **Platform support**: pre-built binaries for Linux x86_64 + aarch64
> (musl static), macOS Intel + Apple Silicon, Windows x86_64; `.deb`
> (amd64 / arm64), `.rpm` (x86_64 / aarch64), Termux `.deb` (aarch64,
> bionic); a multi-arch container (`ghcr.io/lordmacu/nexo-rs:latest`);
> and crates.io (`cargo install nexo-rs`). Every release artifact is
> cosign-signed with a `.sha256` sidecar. Some optional features (STT
> — voice-note transcription) need extra build tools per OS. See
> [Platform support](https://lordmacu.github.io/nexo-rs/docs/getting-started/platform-support.html)
> for the per-OS prerequisites + feature matrix.
>
> A Homebrew formula auto-publish + `npm install -g @nexo-rs/cli`
> are still on the roadmap — the [tap](https://github.com/lordmacu/homebrew-nexo-rs)
> + [npm scope](https://www.npmjs.com/package/@nexo-rs/cli) are reserved.

### From source

```bash
git clone https://github.com/lordmacu/nexo-rs.git
cd nexo-rs
cargo build --release
./target/release/nexo setup
./target/release/nexo --config ./config
```

NATS bus is **optional post-Phase-92**. Subprocess plugins
(WhatsApp, Telegram, marketing) bridge through the daemon's
stdio JSON-RPC channel when `broker.yaml type: local`, so
single-host dev + small-prod setups need no external broker.

Switch to a real NATS instance for multi-host clusters:

```bash
sudo systemctl start nats-server          # or docker run nats:2.10-alpine
nexo set-broker nats --url nats://localhost:4222
# Daemon respawns ~3s later, picks up the new transport.
```

See [broker shapes](./docs/src/architecture/broker-shapes.md)
for when to pick which.

## What you can build

nexo-rs is the **runtime**; you bring the channels, the agents, and
(for SaaS) the product layer on top. A reference build:

### `agent-creator` — operator UI / control-plane microapp

[**lordmacu/agent-creator-microapp**](https://github.com/lordmacu/agent-creator-microapp)
is a [microapp](./docs/src/microapps/getting-started.md) (a Phase 11
nexo extension with its own React UI + HTTP backend) that gives an
operator a graphical control plane over their daemon — create / edit
agent definitions without touching YAML, link & QR-pair WhatsApp
numbers, register LLM provider keys per agent, browse live
conversations, take over a chat manually, see escalation badges. It
talks to the daemon over the admin RPC surface, runs the firehose SSE
stream, and consumes the `@lordmacu/nexo-microapp-ui-react` theme
preset — i.e. it exercises the full microapp contract end to end.
It's also what surfaced (and validated) the Phase 92 stdio-bridge
broker in production: WhatsApp/marketing subprocess plugins, `nexo
set-broker local`, zero NATS server.

### More shapes

WhatsApp sales agent · email-triage agent · Telegram support copilot
with KB + handoff · multi-agent CRM (intake → qualifier → closer) ·
internal ops bot over Slack-via-MCP · browser scraping agent · RSS →
Telegram poller · multi-tenant SaaS where end-users build their own
WhatsApp agents · vertical extension packs (sales / support /
marketing) · persona packs ([Cody](https://github.com/lordmacu/nexo-persona-cody) —
the programmer-pair reference). Each with a recipe / template:
[**What you can build**](https://lordmacu.github.io/nexo-rs/docs/what-you-can-build.html).

## Build on nexo — SDKs

nexo-rs has three extension surfaces; pick by **who owns the runtime
and the UI** (full decision table:
[Plugin authoring overview](https://lordmacu.github.io/nexo-rs/docs/plugins/authoring.html)):

| You're building | Surface | SDK | Operator installs with |
|---|---|---|---|
| A **channel** (Slack, Discord, …), poller, tool, LLM provider or memory backend — code the daemon spawns as a subprocess | **Plugin** | Rust · Python · TypeScript · PHP — one wire contract, four languages | `nexo plugin install <owner>/<repo>` |
| A **bundle** of skills / advisors / prompts / YAML config | **Extension** | YAML + small Rust stubs | `nexo ext install ./<pack>` (local tarball / dir) |
| The **product** on top of nexo-rs (multi-tenant SaaS, internal tool, white-label) — your UI, your domain, your billing | **Microapp** | `nexo-microapp-sdk` (Rust, crates.io) + `@lordmacu/nexo-microapp-ui-react` (npm — React UI kit) | bundled with the product |

### Plugin SDKs

Scaffold a plugin in any of the four languages and push it to GitHub —
a release tarball is all an operator needs. The SDKs are also published
to their language registries (mono-repo:
[`lordmacu/nexo-plugin-sdks`](https://github.com/lordmacu/nexo-plugin-sdks)):

```bash
nexo plugin new my_plugin --lang rust --owner you   # or --lang python|typescript|php
cd my_plugin && nexo plugin run .                   # inner-loop: build + hot-reload, no GitHub round-trip
```

| Language | Package (registry) | Add to an existing project |
|---|---|---|
| **Rust** | [`nexo-microapp-sdk`](https://crates.io/crates/nexo-microapp-sdk) (feature `plugin`) + [`nexo-broker`](https://crates.io/crates/nexo-broker) | `cargo add nexo-microapp-sdk -F plugin && cargo add nexo-broker` |
| **Python** | [`nexoai`](https://pypi.org/project/nexoai/) (import name stays `nexo_plugin_sdk`) | `pip install nexoai` |
| **TypeScript** | [`nexo-plugin-sdk`](https://www.npmjs.com/package/nexo-plugin-sdk) | `npm install nexo-plugin-sdk` |
| **PHP** | [`nexo/plugin-sdk`](https://packagist.org/packages/nexo/plugin-sdk) | `composer require nexo/plugin-sdk` |

Guides: [Plugin quickstart (10 min)](https://lordmacu.github.io/nexo-rs/docs/plugins/quickstart.html) ·
[Authoring overview](https://lordmacu.github.io/nexo-rs/docs/plugins/authoring.html) ·
[Plugin contract — the wire spec all four implement](https://lordmacu.github.io/nexo-rs/docs/plugins/contract.html) ·
per-language: [Rust](https://lordmacu.github.io/nexo-rs/docs/plugins/rust-sdk.html) ·
[Python](https://lordmacu.github.io/nexo-rs/docs/plugins/python-sdk.html) ·
[TypeScript](https://lordmacu.github.io/nexo-rs/docs/plugins/typescript-sdk.html) ·
[PHP](https://lordmacu.github.io/nexo-rs/docs/plugins/php-sdk.html).

### Microapp SDK

`nexo-microapp-sdk` (crates.io) is the Rust SDK for the **product
layer** — feature-gated modules for voice / STT / wizard / events /
admin RPC over the daemon. Start from the bundled `template-microapp-rust`
template (it depends on the published crate, so a copied-out tree builds
standalone); the React frontend kit ships as
[`@lordmacu/nexo-microapp-ui-react`](https://www.npmjs.com/package/@lordmacu/nexo-microapp-ui-react).
Guide: [Microapps · getting started (1-hour walkthrough)](https://lordmacu.github.io/nexo-rs/docs/microapps/getting-started.html).

**Worked example — [`lordmacu/agent-creator-microapp`](https://github.com/lordmacu/agent-creator-microapp):**
the operator control-plane microapp described above (React UI + HTTP
backend over the admin RPC + firehose SSE, `@lordmacu/nexo-microapp-ui-react`
theme preset, the Phase 92 local-broker shape in production). The most
complete reference for exercising the microapp contract end to end.

## Configuration

### Public vs. private agents

The runtime loads the agent catalog from two places and merges them:

1. `config/agents.yaml` — checked into git, safe to publish
2. `config/agents.d/*.yaml` — **gitignored** drop-in directory for
   anything with business content (full sales prompts, pricing tables,
   internal phone numbers)

Drop-in files share the same top-level shape:

```yaml
# config/agents.d/ana.yaml   (gitignored — NOT committed)
agents:
  - id: ana
    model:
      provider: anthropic
      model: claude-haiku-4-5
    plugins:
      - whatsapp
    inbound_bindings:
      - plugin: whatsapp
    allowed_tools:
      - whatsapp_send_message
    outbound_allowlist:
      whatsapp:
        - "573000000000"
    system_prompt: |
      (your sales flow, tarifario, anti-manipulation rules…)
```

Files load in alphabetical order so `00-dev.yaml` / `10-prod.yaml`
layering works as you'd expect.

A `config/agents.d/ana.example.yaml` template ships in the repo so the
shape stays discoverable.

### Per-agent sandboxing

Every agent config can pin itself to a subset of the registered tools
and channels:

```yaml
allowed_tools:          # empty = everything; populated = strict allowlist
  - memory_*
  - whatsapp_send_message
inbound_bindings:       # which channels may trigger this agent
  - plugin: whatsapp
outbound_allowlist:     # defense-in-depth for send tools
  whatsapp: ["573000000000"]
  telegram: [1194292426]
```

`allowed_tools` is enforced by dropping non-matching tools from the
registry at build time — the LLM never even sees them.

### Secrets

Secrets live under `./secrets/` (gitignored), Docker's
`/run/secrets/`, or `CONFIG_SECRETS_DIR`. YAML references them with
`${file:./secrets/xxx}` or `${ENV_VAR}`. Path traversal (`..`) is
rejected at parse time.

## Channels

| Channel | Inbound | Outbound tools |
|---|---|---|
| WhatsApp | `plugin.inbound.whatsapp` | `whatsapp_send_message`, `whatsapp_send_reply`, `whatsapp_send_reaction`, `whatsapp_send_media` |
| Telegram | `plugin.inbound.telegram` | `telegram_send_message`, `telegram_send_reply`, `telegram_send_reaction`, `telegram_edit_message`, `telegram_send_location`, `telegram_send_media` |
| Email | via `gmail-poller` → outbound events | polling + regex extraction, cron-style |
| Browser | `plugin.outbound.browser` | full CDP family (`browser_*`) |

WhatsApp pairing is setup-time only — the runtime refuses to boot
without valid creds and prints a single-line hint pointing at the
wizard. Same pattern for Google OAuth.

## LLM providers

- **Anthropic** — API key or subscription (OAuth PKCE)
- **MiniMax M2.5** — primary on the Anthropic-flavour proxy
- **OpenAI-compatible** — anything that speaks that wire (Ollama,
  Groq, etc.)
- **Gemini** — via the native `google_*` tool family for Gmail /
  Calendar / Drive / Sheets

Rate limiting and retry are per-provider; 429s back off exponentially
with jitter up to 60 s.

## Operational guardrails

- **Single-instance lockfile** at `data/agent.lock`. Starting a second
  process automatically terminates the first — no more two-agents-on-
  one-NATS duplicate-message bugs.
- **Skill gating.** A skill declaring `requires: env: [FOO]` is dropped
  entirely when `FOO` is unset rather than told to the LLM as an
  available capability.
- **Inbound filter.** Events with neither text nor media (receipts,
  reactions, typing indicators) never reach the LLM.
- **Disk queue drain** on NATS reconnect so offline messages aren't
  lost or delivered twice.
- **Dead-letter queue** for events that exceed retry budget, with CLI
  inspect / replay / purge.
- **Metrics** on `:9090/metrics` (Prometheus), health on `:8080`,
  admin console on `127.0.0.1:9091`.

## Development

Building **on** nexo-rs (plugins / extensions / microapps)? See
[**Build on nexo — SDKs**](#build-on-nexo--sdks) above. This section
is for hacking on **the daemon itself**:

```bash
cargo build --workspace            # whole tree
cargo nextest run                  # parallel test runner
cargo build --profile release-fast # release-grade, skip LTO — ~50% faster
cargo run --bin nexo -- --help     # subcommands (init, setup, plugin, persona, ext, flow, dlq, set-broker, mcp-server, …)
```

See [`CHANGELOG.md`](CHANGELOG.md) for what's shipped per release
and [Contributing](https://lordmacu.github.io/nexo-rs/docs/contributing.html)
for the PR workflow.

## FAQ

### Is nexo-rs an OpenClaw alternative?

Yes. nexo-rs is a **Rust alternative to OpenClaw**. It keeps the
parts of OpenClaw that matter — the multi-channel agent gateway,
the plugin SDK, the "agents on real messengers" thesis — and
rebuilds them on Rust + tokio + NATS for memory safety, true
parallelism, single-binary deployment, and built-in fault tolerance
(circuit breaker, disk queue, DLQ, hot reload).

### How does nexo-rs compare to OpenClaw?

Same shape, different substrate. Full table: [side-by-side](#side-by-side).
Highlights: a single ~90 MB binary (~15 MB to download) vs. Node +
`pnpm install`, zero-config boot vs. wire-config-first, NATS *or* a
local stdio bridge (no external server) vs. in-memory only, per-agent
capability sandbox vs. global plugin allowlist, both sides of MCP vs.
client-only, durable TaskFlow workflows, Claude subscription OAuth,
plugin SDKs in 4 languages, Termux-friendly.

### Can I run nexo-rs on a Raspberry Pi or Termux phone?

Yes. nexo-rs ships as a **single static Rust binary** (~90 MB built,
~12-16 MB to download as an xz tarball; aarch64 `.deb` for Termux).
No Node, no Docker, no system packages required. ARM Termux without
root works.

### Which LLM providers does nexo-rs support?

**Anthropic Claude** (API key **and** OAuth PKCE — reuse your
Claude Code subscription quota), **MiniMax M2.5**,
**OpenAI-compatible** endpoints (Ollama, Groq, vLLM, …), and
**Gemini** via the native `google_*` tool family.

### Does nexo-rs speak MCP (Model Context Protocol)?

Both sides. **MCP client** over stdio and HTTP, **and** the agent
itself can run as an **MCP server** (`agent mcp-server`) so other
LLM hosts (Claude Desktop, Claude Code, etc.) can call your agents
as tools.

### Which messaging channels are supported?

**WhatsApp** (Signal Protocol via `whatsapp-rs`, QR pairing),
**Telegram** (full bot API: messages, replies, reactions, edits,
location, media), **Gmail** (poller + outbound), and **browser
control** via Chrome DevTools Protocol.

### Is nexo-rs production-ready?

Yes — **v0.1.6 GA**. Core runtime, NATS/local broker with disk
queue + DLQ + circuit breaker, the full LLM stack, memory + vector,
WhatsApp / Telegram / Email / Browser channels, heartbeat,
agent-to-agent, MCP (both sides), TaskFlow, Claude OAuth, hot reload,
secrets audit, plus the v0.1.6 ergonomics (zero-config boot,
`nexo init`, `nexo set-broker`, `nexo plugin install`) all shipped
end-to-end with a cosign-signed multi-platform release pipeline.
Used in production for real WhatsApp / Telegram sales bots and
personal-assistant agents.

## Looking for an OpenClaw alternative?

If you landed here searching for **"OpenClaw Rust"**,
**"OpenClaw alternative"**, **"WhatsApp LLM agent Rust"**,
**"Telegram bot framework Rust"**, **"multi-agent NATS"**,
**"MCP server Rust"** or **"Claude subscription OAuth agent
framework"** — nexo-rs is for you. Read [Inspired by OpenClaw,
supercharged by Rust](#inspired-by-openclaw-supercharged-by-rust)
or jump straight to the [Quick start](#quick-start).

> **Suggested GitHub topics** (for repo discovery): `rust`,
> `agent-framework`, `multi-agent`, `llm`, `openclaw`,
> `openclaw-alternative`, `whatsapp-bot`, `telegram-bot`,
> `gmail-bot`, `mcp`, `mcp-server`, `nats`, `anthropic`, `claude`,
> `claude-code`, `minimax`, `tokio`, `chrome-devtools-protocol`,
> `taskflow`, `termux`, `self-hosted`.

## License

Dual-licensed under either:

- MIT — see [`LICENSE-MIT`](LICENSE-MIT)
- Apache-2.0 — see [`LICENSE-APACHE`](LICENSE-APACHE)

at your option. SPDX: `MIT OR Apache-2.0`.

Redistributions must preserve the attribution in [`NOTICE`](NOTICE)
per Section 4(d) of the Apache License.
