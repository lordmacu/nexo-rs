# nexo-rs — Rust multi-agent LLM framework (OpenClaw alternative)

> **The Rust alternative to [OpenClaw](https://github.com/openclaw/openclaw).**
> Multi-agent LLM gateway for WhatsApp, Telegram, Gmail and the browser —
> single static binary, NATS-backed, fault-tolerant, MCP-native.

[![docs](https://img.shields.io/badge/📘%20docs-lordmacu.github.io%2Fnexo--rs-blue?style=for-the-badge)](https://lordmacu.github.io/nexo-rs/)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green?style=for-the-badge)](#license)
[![rust](https://img.shields.io/badge/built%20with-Rust-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![openclaw alternative](https://img.shields.io/badge/OpenClaw-alternative-purple?style=for-the-badge)](#inspired-by-openclaw-supercharged-by-rust)

**📘 Full documentation: <https://lordmacu.github.io/nexo-rs/>**

[Quick start](https://lordmacu.github.io/nexo-rs/getting-started/quickstart.html) ·
[Architecture](https://lordmacu.github.io/nexo-rs/architecture/overview.html) ·
[Recipes](https://lordmacu.github.io/nexo-rs/recipes/index.html) ·
[vs OpenClaw](https://lordmacu.github.io/nexo-rs/architecture/vs-openclaw.html) ·
[Contributing](./docs/src/contributing.md) ·
[Security](./SECURITY.md) ·
[License](#license)

**nexo-rs** is a **Rust multi-agent LLM framework** — an
**OpenClaw alternative in Rust** — for building **WhatsApp bots**,
**Telegram bots**, **Gmail pollers** and **browser agents** behind a
single binary. Event-driven by **NATS**, with per-agent tool
sandboxes, **MCP** client + server, **Claude / Anthropic / MiniMax /
OpenAI-compatible** providers, durable workflows (TaskFlow), and
drop-in configuration for private vs. public agents.

One process, many agents, many channels. Kate handles your personal
Telegram; Ana works the WhatsApp sales line; a cron-style poller sweeps
Gmail for leads — all sharing one broker, one tool registry, one
memory layer.

**Keywords:** OpenClaw alternative · Rust agent framework · multi-agent
LLM · WhatsApp LLM bot · Telegram AI agent · Gmail agent · MCP server
Rust · NATS agent gateway · Claude Code subscription · Anthropic OAuth
PKCE · agent framework Termux · self-hosted AI agents.

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

- **Single static binary, ~34 MB.** No `pnpm install`, no `node_modules`,
  no runtime VM. Drop the binary on a Termux phone, a Raspberry Pi,
  a Proxmox container — it runs.
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
| Language / runtime | TypeScript on Node 22+ | Rust, no runtime — single binary |
| Install footprint | `pnpm install` (~42 runtime deps) + Node | one **34 MB** binary (29 MB stripped, 13 MB gzipped) |
| Process model | single Node process | multi-process via NATS broker; in-process fallback when broker is offline |
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
| Bundled skills/extensions | many in `extensions/` | 22 skills + 30+ extensions (browser, gmail-poller, 1password, proxmox, ssh-exec, brave-search, wolfram-alpha, …) |

**What we did not improve.** OpenClaw has a richer installer flow,
a longer track record, and a TypeScript surface that's familiar to
a wider audience. If your shop is JS-first, picking it up is
faster than learning Rust. nexo-rs trades that approachability for
the operational properties above.

Full side-by-side: [docs → vs-openclaw](https://lordmacu.github.io/nexo-rs/architecture/vs-openclaw.html).

## Status

| Phase | Area | Done |
|---|---|:---:|
| 1 | Core runtime + session manager | ✅ |
| 2 | NATS broker + disk queue + DLQ + circuit breaker | ✅ |
| 3 | LLM integration (Anthropic, MiniMax, OpenAI-compat) | ✅ |
| 4 | Browser control (Chrome DevTools Protocol) | ✅ |
| 5 | Memory (short-term, SQLite long-term, sqlite-vec) | ✅ |
| 6 | WhatsApp plugin | ✅ |
| 7 | Heartbeat + reminders | ✅ |
| 8 | Agent-to-agent routing + delegation | ✅ |
| 9 | Metrics, health, graceful shutdown, Docker | ✅ |
| 10 | Soul/identity/dreaming/workspace-git | ✅ |
| 11 | Extension system (stdio + NATS) | ✅ |
| 12 | MCP (client + server) | ✅ |
| 13 | 22 skills/extensions shipped out of the box | ✅ |
| 14 | TaskFlow runtime (durable multi-step flows) | ✅ |
| 15 | Claude subscription auth (OAuth PKCE + API keys) | ✅ |

Total: 113/113 sub-phases.

## Architecture

```
              ┌──────────────────┐
              │   NATS broker    │◄── fault-tolerant event bus
              └──┬────────────┬──┘       (disk queue + DLQ
 plugin.inbound.*│            │plugin.outbound.*   when offline)
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

Requires Rust (2021 edition) and a running NATS instance
(`docker run -p 4222:4222 nats:2.10-alpine` is enough to start).

```bash
git clone git@github.com:lordmacu/nexo-rs.git
cd nexo-rs
cargo build --release

# Pair channels, pick a provider, consent to Google, etc.
./target/release/agent setup

# Run
./target/release/agent --config ./config
```

First run will tell you exactly what's missing (API keys, paired
WhatsApp, etc.) instead of failing silently.

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

```bash
cargo build --workspace           # whole tree
cargo test --workspace --lib      # unit tests
cargo run --bin agent -- --help   # subcommands (dlq, ext, flow, setup)
```

The project uses a `/forge` convention:
`brainstorm → spec → plan → ejecutar`. Per-sub-phase done-criteria
live in [`proyecto/PHASES.md`](proyecto/PHASES.md).

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
Highlights: 34 MB single binary vs. Node + `pnpm install`,
multi-process via NATS vs. single Node process, per-agent
capability sandbox vs. global plugin allowlist, both sides of MCP
vs. client-only, durable TaskFlow workflows, Claude subscription
OAuth, Termux-friendly.

### Can I run nexo-rs on a Raspberry Pi or Termux phone?

Yes. nexo-rs ships as a **single static Rust binary** (~34 MB,
13 MB gzipped). No Node, no Docker, no system packages required.
ARM Termux without root works.

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

113 / 113 sub-phases shipped across 15 phases (core runtime,
NATS broker with disk queue + DLQ + circuit breaker, LLM stack,
memory, WhatsApp, Telegram, heartbeat, agent-to-agent, metrics,
hot reload, MCP, TaskFlow, Claude OAuth). Used in production for
real WhatsApp/Telegram sales bots and personal-assistant agents.

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
