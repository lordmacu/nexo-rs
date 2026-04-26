# nexo-driver-permission

The MCP server that gates every tool call Claude Code wants to
make inside a goal. Without this crate, Claude in headless mode
would refuse to act (the CLI requires `--permission-prompt-tool`
to be set when running unattended).

## What it does

Claude Code is launched by `driver-loop` with:

```
claude --output-format stream-json \
       --permission-prompt-tool mcp__nexo-driver__permission_prompt \
       --mcp-config <path>
```

When Claude proposes a tool call (`Edit`, `Bash`, `Write`,
etc.), it first invokes the MCP tool defined here. The MCP
server hands the request to a `PermissionDecider`. The
decider's verdict (`AllowOnce` / `AllowSession` / `Deny`) flows
back to Claude, which only executes if allowed.

## What's in here

| Type | Purpose |
|---|---|
| `PermissionRequest { goal_id, tool_use_id, tool_name, input, metadata }` | What Claude wants to do. |
| `PermissionResponse { tool_use_id, outcome, rationale }` | What we tell it. |
| `PermissionOutcome { AllowOnce { updated_input? }, AllowSession { scope, updated_input? }, Deny { message } }` | Three verdicts. `AllowSession` cached so a re-call within the same scope skips the decider; `Deny.message` flows into Claude's prompt as user feedback. |
| `AllowScope { Tool, Session, Process }` | How wide an `AllowSession` cache key extends. |
| `PermissionDecider` trait | `async fn decide(req) -> PermissionResponse`. The single thing the orchestrator parameterises. |
| `AllowAllDecider` | Dev-only: every request → `AllowOnce`. The bin emits a loud `tracing::warn!` on boot so an operator who ran the server standalone notices. |
| `DenyAllDecider { reason }` | Shadow-mode (Phase 67.11): Claude proposes but never acts. Useful for calibration runs. |
| `ScriptedDecider` | Test fixture — pre-populated queue of `PermissionOutcome` values. Errors when exhausted to keep tests honest. |
| `nexo-driver-permission-mcp` (bin) | The MCP server. Exposes one tool, `permission_prompt`, that wraps the trait. Reads requests from a Unix socket if `NEXO_DRIVER_DECISION_SOCKET` is set; otherwise consults a static `AllowAllDecider`. |
| `DriverSocketServer` (in `socket.rs`) | The Unix-socket bridge `driver-loop` binds at boot. Forwards permission requests to its `Arc<dyn PermissionDecider>`; the MCP bin connects on the other end. |

## Why a separate crate

The MCP tool is a CLI contract — `claude` discovers it by
spawning the bin and reading its stdio JSON-RPC. Putting the
bin in its own crate means:

- The bin doesn't pull in `tokio::sync::watch` /
  `nexo-driver-loop` / hooks. Stays small.
- Operators can run it standalone for debugging
  (`nexo-driver-permission-mcp --allow-all`).
- Future Phase 67.11 shadow-mode swap is one config flag, not
  a binary rebuild.

## Where it sits

```
   ┌────────────────────┐
   │  Claude subprocess │
   │  (in worktree)     │
   └─────────┬──────────┘
             │ MCP stdio JSON-RPC
   ┌─────────▼──────────┐
   │  nexo-driver-      │   reads `NEXO_DRIVER_DECISION_SOCKET`
   │  permission-mcp    │
   │  (this bin)        │
   └─────────┬──────────┘
             │ unix socket
   ┌─────────▼──────────┐
   │  DriverSocketServer│   bound by driver-loop at boot
   │  (in this crate)   │
   └─────────┬──────────┘
             │ trait call
   ┌─────────▼──────────┐
   │  PermissionDecider │   AllowAll / DenyAll / Scripted /
   │                    │   future LlmDecider (Phase 67.4)
   └────────────────────┘
```

## See also

- `architecture/project-tracker.md` — overall flow.
- `architecture/driver-subsystem.md` — Phase 67.3 details.
