# Why nexo-rs

If you've tried to build a real agent system, you know the gap.

Most "agent frameworks" assume **one** LLM talking to **one** user
through **one** UI. Real deployments are not shaped that way.

You have several agents with different personas, models, and skills.
Multiple channels (WhatsApp + Telegram + mail) feed the same agents.
Business logic that is **not** LLM-driven (scheduled tasks, regex
email triage, lead notifications) runs next to the LLM loop. Private
prompts and pricing tables live alongside an open-source core. Customer
support agents need a different tool sandbox than your billing bot.

nexo-rs is opinionated toward that shape.

## What you get

- **One process, many agents, many channels.** Kate handles your
  personal Telegram. Ana works the WhatsApp sales line. A cron-style
  poller sweeps Gmail for leads — all sharing one broker, one tool
  registry, and one memory layer.
- **Single 34 MB binary.** No Node, no npm, no Docker required.
  Stripped: 29 MB. Gzipped: 13 MB. Runs on a fresh VPS, on Termux
  without root, or as a systemd unit.
- **Production-grade by default.** NATS-backed event bus with disk
  fallback. Per-agent capability sandboxes. Cosign-verified plugin
  marketplace. Multi-tenant SaaS-ready.

## Three layers of extensibility

When you're ready to add functionality, pick the right layer:

| Layer | Use it when | Ships as |
|-------|-------------|----------|
| [**Plugin**](./plugins/contract.md) | You want a new **channel** (Discord, Slack, custom protocol) or to expose new **tools** to agents. | Subprocess in your favorite language (Rust, Python, TypeScript, PHP), tarball published via GitHub Releases. |
| [**Extension**](./extensions/manifest.md) | You're bundling **tools, advisors, skills, MCP servers** as a self-contained unit — typical for SaaS verticals (sales, support, marketing). | Local-path tarball; operator runs `nexo ext install`. |
| [**Microapp**](./microapps/getting-started.md) | You're building a **SaaS product** on top of nexo-rs. The framework runs in the background; your app owns the UI and the multi-tenant story. | Your own application, talking to nexo-rs over admin RPC (NATS). |

## What it's not

- Not a **chat webapp**. There's no built-in UI; you bring your own
  channels (and your own UI for microapps).
- Not a **single-tenant prototype**. Multi-tenant SaaS is a first-class
  shape, not an afterthought.
- Not a **research toy**. Every release is signed; the install pipeline
  has cosign verification + sha256 checks; the broker has disk fallback;
  the test surface covers all four SDK languages.

## How it compares

The closest reference point is [OpenClaw](https://github.com/openclaw/openclaw)
(TypeScript, Node). nexo-rs trades JS familiarity for:

- A single static Rust binary (vs Node + transitive deps)
- A fault-tolerant NATS broker layer (vs in-memory only)
- Per-agent capability sandboxes
- Durable workflows + secrets audit
- Termux / mobile-first portability
- Plugin SDKs in 4 languages (vs TypeScript only)

See [vs OpenClaw](./architecture/vs-openclaw.md) for the full
side-by-side.

## Where to next

- New here? → [Quickstart](./getting-started/quickstart.md) — install
  + first agent in 5 minutes.
- Want to write a plugin? → [Plugin contract](./plugins/contract.md).
- Building a SaaS? → [Microapps · getting started](./microapps/getting-started.md).
- Curious about internals? → Browse the **Advanced** section in the
  sidebar — architecture deep-dives, ADRs, design notes.
