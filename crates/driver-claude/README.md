# nexo-driver-claude

> Phase 67.1 — spawn the `claude` CLI under `nexo-rs`, consume its `stream-json` output as typed events, and keep session ids around across turns so `--resume <id>` works. The substrate the self-driving agent loop (Phase 67.4) sits on top of.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## Status

**Internal crate — not published to crates.io.** Marked
`publish = false, release = false` in `release-plz.toml`. Lives
inside the workspace as the Claude-specific harness layer of the
Phase 67 driver subsystem.

## Layering

This crate is a *leaf* on top of `nexo-driver-types`:

```
nexo-driver-types  ── trait + Goal/Decision wire types (67.0)
        ▲
        │
nexo-driver-claude ── ClaudeCommand / ClaudeEvent / TurnHandle (67.1)
```

It does NOT depend on `nexo-core`. Higher fases (67.4 driver loop,
67.5 acceptance evaluator, 67.10 escalation) compose this crate
together with `nexo-core`.

## Quick example

```rust,no_run
use std::time::Duration;
use nexo_driver_claude::{ClaudeCommand, spawn_turn};
use nexo_driver_types::CancellationToken;

# async fn doc() -> anyhow::Result<()> {
let cmd = ClaudeCommand::discover("Implement Phase 26.z")?
    .resume("01HZX...")
    .allowed_tools(["Read", "Grep", "Glob", "LS"])
    .permission_prompt_tool("mcp__nexo-driver__permission_prompt")
    .cwd("/tmp/claude-runs/26-z");

let cancel = CancellationToken::new();
let mut turn = spawn_turn(cmd, &cancel, Duration::from_secs(600), Duration::from_secs(1)).await?;

while let Some(event) = turn.next_event().await? {
    // dispatch on event…
    let _ = event;
}
let _exit = turn.shutdown().await?;
# Ok(())
# }
```

## Cross-platform notes

`tokio::process` works on Windows, macOS, and Linux. The full
end-to-end test suite (`tests/spawn_turn_test.rs`) is `#[cfg(unix)]`
because it shells out to a `bash` mock script.

## Sub-phases that build on this

| Phase | What |
|-------|------|
| 67.2 | SQLite-backed `SessionBindingStore` |
| 67.3 | MCP `permission_prompt` tool wired into the spawn args |
| 67.4 | Driver agent loop that owns the `TurnHandle` |
| 67.5 | Acceptance evaluator that fires after `Result::Success` |
