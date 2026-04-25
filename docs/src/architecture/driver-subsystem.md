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

## Sub-phases

| Phase | What | Status |
|-------|------|--------|
| 67.0 | `AgentHarness` trait + types | ✅ |
| 67.1 | `claude_cli` skill (spawn + stream-json + resume) | ✅ |
| 67.2 | Session-binding store (SQLite) | ⬜ |
| 67.3 | MCP `permission_prompt` in-process | ⬜ |
| 67.4 | Driver agent loop + budget guards | ⬜ |
| 67.5 | Acceptance evaluator | ⬜ |
| 67.6 | Git worktree sandboxing + per-turn checkpoint | ⬜ |
| 67.7 | Memoria semántica de decisiones | ⬜ |
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
