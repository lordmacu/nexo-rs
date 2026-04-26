# nexo-project-tracker

> Parses `proyecto/PHASES.md` + `proyecto/FOLLOWUPS.md` into a queryable phase / follow-up index for Phase 67 driver goals.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## Status

**Internal crate — not published to crates.io.** Marked
`publish = false, release = false` in `release-plz.toml`. Lives
inside the workspace as the source-of-truth parser the Phase 67
driver subsystem consults to look up phase metadata when an agent
calls `program_phase(phase_id=...)`.

## What this crate does

- **PhaseParser** — reads `proyecto/PHASES.md`, walks the markdown
  AST, and produces a tree of `Phase { id, title, status, sub_phases }`.
  Handles the project's checkbox conventions (`✅ / 🔄 / ⬜`) and
  nested `### Phase N` / `#### N.x` headings.
- **FollowupsParser** — reads `proyecto/FOLLOWUPS.md` and produces
  one `Followup { id, title, status, body }` per top-level entry
  (P-1, H-1, L-2, …). Recognises the `~~strikethrough~~ ✅ shipped`
  pattern + the `🔄 partial` annotation.
- **Tracker** — caches both files behind a notify-watcher so a
  restart isn't required when the operator edits PHASES.md /
  FOLLOWUPS.md mid-session. Cache-invalidation is mtime-based.
- **Tool format** — single canonical JSON shape the LLM-facing
  tools consume so prompts stay stable when the markdown evolves.
- **Git integration** — when the project is tracked under git,
  the tracker can return the commit history for a given phase to
  show what landed in each.

## Why it's separate from `nexo-core`

The phase index is consumed only by the Phase 67 driver subsystem
(`program_phase`, `chain`, `dispatch_followup` tools — see
`nexo-dispatch-tools`). Splitting it out keeps `nexo-core`'s
dependency tree small for operators who don't run the driver
subsystem; also makes the parser unit-testable in isolation
without spinning up an agent.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
