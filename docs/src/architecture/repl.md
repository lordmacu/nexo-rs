# `Repl` (Phase 79.12)

Stateful REPL tool — spawn persistent Python, Node.js, or bash
subprocesses whose interpreter survives across LLM turns inside the
same goal. Variables, imports, and definitions persist; the model
executes code iteratively without restarting the interpreter each turn.

Feature-gated behind `repl-tool`. Off by default — arbitrary code
execution is dangerous and must be explicitly opted into per agent /
per binding.

Lift from
`upstream agent CLI` +
`src/services/sandbox/`.

## Tool shape

```json
{
  "action":     "spawn | exec | read | kill | list",
  "session_id": "<uuid>",       // required for exec, read, kill
  "runtime":    "python | node | bash",  // required for spawn
  "code":       "print(1+1)",   // required for exec
  "cwd":        "/tmp"          // optional for spawn (defaults to agent workspace)
}
```

| Action | Behaviour |
|--------|-----------|
| `spawn` | Launch a new persistent REPL session. Returns `session_id`. |
| `exec`  | Run code in a running session. Returns `stdout`, `stderr`, `timed_out`, `exit_code`. Output is the _difference_ since the last exec/read — only new bytes. |
| `read`  | Read current output buffers without sending code. |
| `kill`  | Terminate a session by id. |
| `list`  | List all active sessions with runtime, cwd, spawned_at, output_len. |

## Output

```json
{
  "stdout":    "2\n",
  "stderr":    "",
  "timed_out": false,
  "exit_code": null
}
```

`timed_out: true` when exec exceeds `timeout_secs` (default 30 s).
Exit code is `Some(n)` only when the child process has terminated.

## Configuration

```yaml
# Agent-level default
agents:
  - id: ana
    repl:
      enabled: true
      allowed_runtimes: ["python", "node"]
      max_sessions: 3
      timeout_secs: 30
      max_output_bytes: 65536

# Per-binding override (replaces the whole struct)
inbound_bindings:
  - plugin: whatsapp
    repl:
      enabled: true
      allowed_runtimes: ["python"]
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Gate the Repl tool. Off by default. |
| `allowed_runtimes` | `[string]` | `["python","node","bash"]` | Runtimes the agent may spawn. |
| `max_sessions` | u32 | `3` | Maximum concurrent REPL sessions per agent. |
| `timeout_secs` | u32 | `30` | Seconds before an exec returns `timed_out: true`. |
| `max_output_bytes` | u64 | `65536` | Per-session output buffer cap. Oldest bytes dropped when cap reached. |

## Runtimes

| Runtime | Binary | Flags |
|---------|--------|-------|
| `python` | `python3` | `-u` (unbuffered), `-q` (quiet), `-i` (interactive — required when stdin is piped) |
| `node` | `node` | `-i` (interactive), `--no-warnings` |
| `bash` | `bash` | `--norc`, `--noprofile` |

## Session lifecycle

- Sessions are keyed by UUID (returned by spawn).
- Output is buffered per-session with oldest-first truncation at `max_output_bytes`.
- Background reader threads (blocking I/O via `std::thread`) read stdout/stderr.
- `exec` snapshots buffer lengths before sending code and returns only the new bytes.
- `wait_for_new_output` polls every 50 ms until the buffer grows beyond the snapshot or the process dies.
- Dead-process detection (`child.try_wait()`) on every exec/read — returns a clear error if the session has terminated.

## Plan-mode classification

Classified `Bash` (mutating) in
`nexo_core::plan_mode::MUTATING_TOOLS`. Plan-mode-on goals receive a
`PlanModeRefusal` rather than a silent exec.

## Sandbox note

Sandbox enforcement (bubblewrap / firejail / macOS sandbox-exec) is
tracked in the Phase 79.12 spec but deferred to a future sub-phase.
Current implementation trusts the operator to enable `repl-tool` only
for agents that need it, and limits blast radius via `allowed_runtimes`
and `max_output_bytes`.

## Out of scope (deferred)

- **Sandbox integration.** `bwrap` / `firejail` / `sandbox-exec`
  probing and enforcement. Tracked in PHASES.md 79.12 sandbox matrix.
- **`allow_unsandboxed` per-binding toggle.** Locked behind capability
  gate — only via direct YAML edit, not self-config.
- **Last-expression value.** The spec's `value: Option<Value>` field
  (JSON-encoded last expression) is deferred.
- **`bash` language is included** as a pragmatic convenience for
  debugging; the PHASES.md spec says "no shell language" but the
  implementation ships it as the session-sandbox risk is lower than an
  ad-hoc `BashTool` invocation (no filesystem side effects beyond the
  session cwd).

## References

- **PRIMARY**:
  `upstream agent CLI` +
  `upstream agent CLI`
- **SECONDARY**: OpenClaw `research/` — no equivalent
  (`grep -rln "repl\|REPL\|stateful.*sandbox" research/src/` returns
  nothing).
- Implementation: `crates/core/src/agent/repl_registry.rs`,
  `crates/core/src/agent/repl_tool.rs`,
  `crates/config/src/types/repl.rs`.
- Plan + spec: `proyecto/PHASES.md::79.12`.
