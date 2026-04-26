# nexo-driver-types

> Foundational types + `AgentHarness` trait for the Phase 67 driver subsystem. **Leaf crate** with no `nexo-core` dependency — so the contract travels through NATS, gets re-imported by extensions, and is consumed by the admin-ui without pulling in the full daemon.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## Status

**Internal crate — not published to crates.io.** Marked
`publish = false, release = false` in `release-plz.toml`.

## What this crate does

The driver subsystem runs another process (the `claude` CLI in
its first impl, but the trait is generic) under a verifiable
goal. Goals carry budget guards, acceptance criteria, and a
running record of allow / deny decisions. This crate defines
the contract every harness, decider, and orchestrator agree on.

### Trait surface

```rust
#[async_trait]
pub trait AgentHarness: Send + Sync {
    fn id(&self) -> AgentHarnessId;
    fn label(&self) -> &str;
    fn supports(&self, goal_kind: GoalKind) -> bool;
    async fn run_attempt(&self, goal: &Goal, ctx: AttemptContext)
        -> Result<AttemptOutcome, HarnessError>;
    async fn compact(&self, session_id: &str) -> Result<()>;
    async fn reset(&self, session_id: &str) -> Result<()>;
    async fn dispose(&self, session_id: &str) -> Result<()>;
}
```

### Types

| Type | Purpose |
|---|---|
| `Goal { id, phase_id, intent, budget, acceptance, … }` | What we're trying to achieve |
| `BudgetGuards { max_turns, max_tokens, max_wallclock }` | Caps so a runaway goal stops |
| `BudgetUsage` | Running counter the harness updates per turn |
| `AcceptanceCriterion` | Objective verification spec (cargo build, custom command, regex) |
| `AcceptanceVerdict` | Pass / Fail with diagnostics |
| `Decision { tool_call, choice, rationale }` | Record of every permission_prompt decision |
| `DecisionChoice` | `Allow | Deny | RequestEscalation` |
| `AttemptOutcome` | `Done | NeedsRetry | Continue | BudgetExhausted | Cancelled | Escalate` |
| `CancellationToken` | Opaque cancellation handle threaded through every async fn |

### Wire format

Every type derives `serde::{Serialize, Deserialize}` so the
contract serialises cleanly across NATS / IPC / admin-ui
boundaries.

## What does NOT live here

- Concrete harness implementations (`nexo-driver-claude`).
- Decider IPC (`nexo-driver-permission`).
- Orchestrator loop (`nexo-driver-loop`).
- Acceptance evaluator runtime (lives in `driver-loop`).

## Why a separate crate

`AgentHarness` is the smallest stable contract the rest of the
driver subsystem depends on. Keeping it in a leaf crate means
extensions, admin-ui, and any third-party harness can implement
the trait without dragging in `nexo-core` or `tokio` features
they don't need.

## Documentation

- [Driver subsystem](https://lordmacu.github.io/nexo-rs/architecture/driver-subsystem.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
