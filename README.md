# nexo-rs

[![docs](https://img.shields.io/badge/docs-latest-blue)](https://lordmacu.github.io/nexo-rs/)
[![api](https://img.shields.io/badge/api-rustdoc-orange)](https://lordmacu.github.io/nexo-rs/api/)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green)](#license)

[📘 Documentation](https://lordmacu.github.io/nexo-rs/) · [API reference](https://lordmacu.github.io/nexo-rs/api/) · [Contributing](./docs/src/contributing.md) · [License](#license)

A Rust framework for building **multi-agent** LLM systems that live on
real messaging channels — WhatsApp, Telegram, email — instead of a chat
webapp. Event-driven by NATS, per-agent tool sandboxes, drop-in
configuration for private vs. public agents.

One process, many agents, many channels. Kate handles your personal
Telegram; Ana works the WhatsApp sales line; a cron-style poller sweeps
Gmail for leads — all sharing one broker, one tool registry, one
memory layer.

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

## License

Dual-licensed under either:

- MIT — see [`LICENSE-MIT`](LICENSE-MIT)
- Apache-2.0 — see [`LICENSE-APACHE`](LICENSE-APACHE)

at your option. SPDX: `MIT OR Apache-2.0`.

Redistributions must preserve the attribution in [`NOTICE`](NOTICE)
per Section 4(d) of the Apache License.
