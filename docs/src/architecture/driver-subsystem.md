# Driver subsystem (Phase 67)

The driver subsystem turns the `nexo-rs` agent runtime into the
"human in the loop" for **another** agent — typically the Claude Code
CLI. It runs a goal-bound experiment: spawn the external CLI, watch
its tool-use stream, decide allow/deny on every action, feed back
acceptance failures, and stop only when the CLI claims "done" AND
objective verification passes.

This page describes the architectural shape; concrete impl details
live with each sub-phase.

## Why

Claude Code (or any other local CLI agent) is excellent at writing
code, but it sometimes:

- **over-claims completion** — says "done" when tests are red;
- **proposes destructive shell commands** when stuck;
- **forgets** which approaches it already tried and failed.

A second agent — driven by `nexo-rs`, backed by a different LLM
(MiniMax M2.5), with persistent memory — closes those gaps.

## Architecture

```
nexo-rs daemon
│
├─ "claude-driver" agent
│   ├─ LLM: MiniMax M2.5
│   ├─ memory: short_term + long_term + vector + transcripts
│   └─ skills: claude_cli, git_checkpoint, test_runner,
│              acceptance_eval, escalate
│
└─ MCP server (in-process)
    └─ tool: permission_prompt(tool_name, input) → {allow|deny, message}

claude  (subprocess, one per turn)
└─ claude --resume <id>
          --output-format stream-json
          --permission-prompt-tool mcp__nexo-driver__permission_prompt
          --add-dir <worktree>
          --allowedTools "Read,Grep,Glob,LS,WebFetch"
          -p "<turn prompt>"
```

## Termination model

Claude says "done" — driver does NOT trust it. Driver runs the goal's
acceptance criteria (`cargo build`, `cargo test`, `cargo clippy`,
PHASES marker, custom verifiers). Only when all pass is the goal
declared `Done`. Otherwise the failures are folded into the next
turn's prompt: "you said done, but here's what still fails — fix it".

The driver also stops on **budget exhaustion**: max turns, wall-time,
tokens, or consecutive denies. On exhaustion the driver escalates to
the operator (WhatsApp / Telegram via existing channel plugins) with
a state dump.

## Foundational types — `nexo-driver-types`

The contract — `AgentHarness` trait + `Goal` / `Attempt` / `Decision`
/ `AcceptanceCriterion` / `BudgetGuards` types — lives in the leaf
crate `nexo-driver-types`. Every value is `serde`-serializable so the
contract can travel through NATS, get re-imported by extensions, and
power admin-ui dashboards without dragging in the daemon.

## How a turn flows (Phase 67.1)

```rust,no_run
use std::time::Duration;
use nexo_driver_claude::{ClaudeCommand, spawn_turn};
use nexo_driver_types::CancellationToken;

# async fn doc(session_id: String) -> anyhow::Result<()> {
let cmd = ClaudeCommand::discover("Implementa Phase 26.z")?
    .resume(session_id)
    .allowed_tools(["Read", "Grep", "Glob", "LS"])
    .permission_prompt_tool("mcp__nexo-driver__permission_prompt")
    .cwd("/tmp/claude-runs/26-z");

let cancel = CancellationToken::new();
let mut turn = spawn_turn(cmd, &cancel, Duration::from_secs(600), Duration::from_secs(1)).await?;

while let Some(ev) = turn.next_event().await? {
    // dispatch on ev (Assistant tool_use → permission_prompt; Result → done check)
    let _ = ev;
}
let _exit = turn.shutdown().await?;
# Ok(())
# }
```

`next_event` cooperatively races three signals via `tokio::select!`:
the cancel token, the per-turn deadline, and the JSONL stream. Errors
land as `Cancelled`, `Timeout`, `ParseLine`, etc. Cleanup is always
`shutdown()` — `ChildHandle::Drop` is the panic safety net.

## Persistence (Phase 67.2)

`SqliteBindingStore` keeps `(goal_id → claude session_id)` plus
timestamps in a single `claude_session_bindings` table. Two filters
are applied on `get`:

- **idle TTL** — `last_active_at` must be within `idle_ttl` of now;
- **max age** — `created_at + max_age` must be in the future.

Either filter can be `None` (no filter) or `Duration::ZERO` (alias).

Three soft-delete-friendly operations live alongside `clear`:

- `mark_invalid(goal_id)` flips `last_session_invalid = 1` instead of
  deleting the row. Phase 67.8 (replay-policy) calls this when Claude
  rejects a session id mid-turn; the row stays for forensics.
- `touch(goal_id)` bumps `last_active_at` only. Driver loop calls it
  per observed event so the idle filter doesn't need a structural
  upsert per turn.
- `purge_older_than(cutoff)` reaps rows the operator no longer cares
  about. Phase 67.6 (worktree janitor) calls it nightly.

Schema migrations: `PRAGMA user_version = 1` is the sentinel; every
`open()` runs `CREATE TABLE/INDEX IF NOT EXISTS`. Future v2 will
extend that helper.

## Permission flow (Phase 67.3)

Every Claude tool call that isn't on the static allowlist
(`Read,Grep,Glob,LS,WebFetch`) goes through the MCP server before
execution:

```
Claude Code ─── tools/call mcp__nexo-driver__permission_prompt ───▶
                                                                    │
                                                          stdio JSON-RPC
                                                                    │
                                                                    ▼
                                              nexo-driver-permission-mcp (child)
                                                                    │
                                                            calls PermissionDecider
                                                                    │
                                                                    ▼
                                                     {behavior: allow|deny, ...}
```

`PermissionMcpServer` exposes one tool, `permission_prompt`. The
in-process `AllowSession` cache keyed on `(tool_name, hash(input))`
short-circuits repeat calls (a Claude turn that re-reads the same
file pays the decider once).

Outcomes Claude receives are always one of two shapes:

```json
{ "behavior": "allow" }                   // optional updatedInput
{ "behavior": "deny", "message": "..." }
```

Internally the driver tracks five outcomes — `AllowOnce`,
`AllowSession{scope}`, `Deny`, `Unavailable`, `Cancelled` — collapsing
the last three to `deny` on the wire. `Unavailable` (timeout) is
fail-closed by design.

Phase 67.3 ships the bin in placeholder modes (`--allow-all` for dev,
`--deny-all <reason>` for shadow). Phase 67.4 will swap those flags
for `--socket <path>` so the bin asks the daemon's `LlmDecider`
(MiniMax + memory) for each decision.

## Goal lifecycle (Phase 67.4)

```
nexo-driver run goal.yaml
        │
        ▼
DriverOrchestrator::run_goal
        │
        ├─ workspace_manager.ensure(&goal)        ─┐
        │                                          │
        ├─ write_mcp_config(workspace,             ├─ side-effects in
        │     bin_path, socket_path)               │   <workspace>/
        │                                          │
        ├─ DriverSocketServer (already running) ──┘
        │     spawned by builder, owned via JoinHandle
        │
        └─ for each turn:
             ├─ budget.is_exhausted? → BudgetExhausted{axis}
             ├─ AttemptStarted event
             ├─ run_attempt(ctx, params)
             │     spawn `claude --resume <id> ... --mcp-config ...`
             │     event-loop on stream-json
             │     binding_store.upsert(session_id)
             │     acceptance.evaluate(criteria, workspace)
             │     return AttemptResult { outcome }
             ├─ AttemptCompleted event
             └─ match outcome:
                Done            → break, GoalCompleted{Done}
                NeedsRetry{f}   → next turn with prior_failures
                Continue{...}   → next turn (e.g. session-invalid retry)
                Cancelled       → break
                BudgetExhausted → break
                Escalate{r}     → emit Escalate event, break
```

`AttemptOutcome::Continue` covers two cases the loop treats the same:
the stream ended without `Result::Success` (Claude crashed early),
and a `session not found` reply that triggered
`binding_store.mark_invalid` so the next turn starts fresh.

NATS subjects emitted (when `feature = "nats"` and
`emit_nats_events: true`):

- `agent.driver.goal.{started,completed}`
- `agent.driver.attempt.{started,completed}`
- `agent.driver.decision`           (Phase 67.7 will populate when
                                     `LlmDecider` records its rationale)
- `agent.driver.acceptance`
- `agent.driver.budget.exhausted`
- `agent.driver.escalate`

## Sub-phases

| Phase | What | Status |
|-------|------|--------|
| 67.0 | `AgentHarness` trait + types | ✅ |
| 67.1 | `claude_cli` skill (spawn + stream-json + resume) | ✅ |
| 67.2 | Session-binding store (SQLite) | ✅ |
| 67.3 | MCP `permission_prompt` in-process | ✅ |
| 67.4 | Driver agent loop + budget guards | ✅ |
| 67.5 | Acceptance evaluator | ✅ |
| 67.6 | Git worktree sandboxing + per-turn checkpoint | ✅ |
| 67.7 | Memoria semántica de decisiones | ✅ |
| 67.8 | Replay-policy (resume tras crash mid-turn) | ⬜ |
| 67.9 | Compact opportunista | ⬜ |
| 67.10 | Escalación a WhatsApp/Telegram | ⬜ |
| 67.11 | Shadow mode (calibración) | ⬜ |
| 67.12 | Multi-goal paralelo | ⬜ |
| 67.13 | Cost dashboard + admin-ui A4 tile | ⬜ |

## See also

- `crates/driver-types/README.md` — contract surface and layering
- `proyecto/PHASES.md` — Phase 67 sub-phase status of record
- OpenClaw reference: `research/src/agents/harness/types.ts`
- OpenClaw subprocess pattern:
  `research/extensions/codex/src/app-server/transport-stdio.ts`
