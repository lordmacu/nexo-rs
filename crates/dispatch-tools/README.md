# nexo-dispatch-tools

> LLM-facing dispatch tools + completion hooks for Phase 67 driver goals.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## Status

**Internal crate — not published to crates.io.** Marked
`publish = false, release = false` in `release-plz.toml`. Lives
inside the workspace as the LLM-side surface of the Phase 67
driver subsystem. Will stabilise + publish once Phase 67.x
finishes the security review.

## What this crate does

- **`program_phase` tool** — entry point an agent calls to start
  a phase against the project tracker. Validates the phase id,
  consults the dispatch policy, builds a `Goal` with the phase's
  acceptance criteria + budget guards, and hands it to the
  driver loop.
- **`agent_control` tool** — `cancel | pause | resume |
  update_budget` operations against in-flight goals. Per-action
  capability gating + sender-trust check.
- **`dispatch_followup` tool** — chains a follow-up phase off a
  previous goal's success / failure, preserving origin context.
- **`chain` tool** — programs an entire ordered list of phases as
  a single dispatch envelope so a long workflow doesn't need
  N round-trips through the LLM.
- **HookRegistry + idempotency store** — completion hooks
  (notify_origin / notify_channel / chain_next) fire when a goal
  reaches a terminal state. Idempotent so retries don't double-
  fire.
- **DispatchGate** — policy + capability + trust check shared
  across every entry point above.

## Architecture

```
LLM                                         Agent runtime
 │                                                 │
 │ tool_call: program_phase(phase_id="13.5")       │
 ├─────────────────────────────────────────────────►│
 │                                                  │
 │              ┌──────────────────────┐           │
 │              │  DispatchGate        │           │
 │              │  policy + caps + …   │           │
 │              └──────────┬───────────┘           │
 │                         │                        │
 │              ┌──────────▼───────────┐           │
 │              │  AgentRegistry       │  (admit)  │
 │              │  cap + FIFO queue    │           │
 │              └──────────┬───────────┘           │
 │                         │                        │
 │              ┌──────────▼───────────┐           │
 │              │  DriverOrchestrator  │           │
 │              └──────────┬───────────┘           │
 │                         │                        │
 │              ┌──────────▼───────────┐           │
 │              │  Driver loop         │           │
 │              │  acceptance, hooks   │           │
 │              └──────────────────────┘           │
```

## Why it's separate from `nexo-core`

The dispatch tools are the LLM-side API surface for Phase 67;
they need to be replaceable per deployment (different operators
will want different policies / caps / hook sets). Living in their
own crate behind a feature gate keeps `nexo-core` minimal for
deployments that don't run the driver subsystem.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
