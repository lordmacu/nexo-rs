# nexo-agent-registry

> In-memory + SQLite-backed registry tracking every in-flight Phase 67 driver goal.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## Status

**Internal crate — not published to crates.io.** Lives inside the
workspace as part of the Phase 67 driver subsystem. Marked
`publish = false, release = false` in `release-plz.toml`. Surface
will stabilise + the crate will be published once Phase 67.x
closes.

## What this crate does

- **AgentHandle** — `(goal_id, phase_id, status, origin,
  dispatcher, started_at, finished_at, snapshot)` for every
  in-flight or recently-finished driver goal.
- **AgentRunStatus** — state machine: `Running | Queued | Paused
  | Done | Failed | Cancelled | LostOnRestart`. Knows which
  states are terminal.
- **AgentSnapshot** — live per-goal stats (turn N/M, last
  acceptance result, last decision summary, last diff_stat,
  last_event_at). Refreshed by the event subscriber that the
  driver loop wires.
- **SqliteAgentRegistryStore** — durable backing so the daemon
  can answer "what is agent X doing?" across restarts. Idempotent
  upserts, list-by-status, status transitions.
- **Cap + FIFO queue** — bounded admission so a runaway dispatcher
  can't queue 1000 simultaneous goals.

## Why it's separate from `nexo-core`

The runtime intake hot path doesn't need to know about driver-
specific concepts (goal IDs, dispatch policies, phase IDs). The
registry is consumed only by the dispatch tools (`nexo-dispatch-
tools`), the driver loop (`nexo-driver-loop`), and the admin
query / control / completion-hook tools. Splitting it out keeps
`nexo-core`'s dependency tree small for operators who don't run
the driver subsystem.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
