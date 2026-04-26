# nexo-driver-loop

> Goal orchestrator for the Phase 67 driver subsystem — runs one goal end-to-end through `ClaudeHarness`, with `LlmDecider`-backed `permission_prompt` arbitration + acceptance-criteria evaluation + git-worktree sandboxing + replay-on-crash semantics.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## Status

**Internal crate — not published to crates.io.** Marked
`publish = false, release = false` in `release-plz.toml`. Lives
inside the workspace as the orchestration core of the Phase 67
driver subsystem (which lets a Nexo agent self-drive the Claude
CLI under verifiable goals).

## What this crate does

- **`DriverOrchestrator::run_goal`** — single entry point that
  closes 67.0–67.9 work end-to-end:
  1. Builds a `ClaudeHarness` (impl `AgentHarness` from
     `nexo-driver-types`).
  2. Spawns Claude via `claude_cli` skill (67.1).
  3. Streams `permission_prompt` decisions through
     `LlmDecider` (consults MiniMax via `nexo-llm`).
  4. Evaluates acceptance criteria after each attempt
     (cargo build/test/clippy + custom verifiers) — 67.5.
  5. Sandboxes file writes in a per-turn git worktree (67.6).
  6. Persists turn state so a crash mid-run resumes cleanly
     (67.8).
  7. Records decisions semantically into long-term memory so
     rejected approaches feed forward (67.7).
- **`ClaudeHarness`** — concrete `AgentHarness` impl tying
  Claude's session resume + acceptance failures + budget
  guards together.
- **`LlmDecider`** — production `PermissionDecider`. Reads the
  tool-call request, consults the LLM with a structured
  rubric, returns `Allow | Deny | RequestEscalation`.
- **`DriverSocketServer`** — daemon-side IPC over a Unix
  socket. Pairs with `SocketDecider` in
  `nexo-driver-permission` so the deciding logic stays in the
  daemon (one model, shared rate limit, audit trail) while
  the MCP child Claude spawns can still ask "is this allowed?"
- **Pause / resume / cancel / update_budget** — runtime
  control surface (Phase 67.G.2) so admin tools can intervene
  on a long-running goal.

## Public API

```rust
pub struct DriverOrchestrator { /* … */ }

impl DriverOrchestrator {
    pub async fn run_goal(&self, goal: Goal) -> Result<AttemptOutcome>;
    pub async fn pause(&self, goal_id: GoalId) -> Result<()>;
    pub async fn resume(&self, goal_id: GoalId) -> Result<()>;
    pub async fn cancel(&self, goal_id: GoalId) -> Result<()>;
    pub async fn update_budget(&self, goal_id: GoalId, max_turns: Option<u32>) -> Result<()>;
}
```

## Why a separate crate

The Phase 67 driver subsystem is large and operationally distinct
from the conversational agent runtime. Splitting it into
`driver-types` (contracts), `driver-permission` (decider IPC),
`driver-claude` (Claude-specific harness), and `driver-loop`
(orchestrator) keeps `nexo-core`'s dependency tree small for
operators who run only conversational agents.

## Documentation

- [Driver subsystem](https://lordmacu.github.io/nexo-rs/architecture/driver-subsystem.html)
- [Project tracker + dispatch](https://lordmacu.github.io/nexo-rs/architecture/project-tracker.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
