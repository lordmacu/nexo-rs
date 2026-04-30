# Brainstorm: Phase 79.12 — REPLTool (Python/Node stateful sandbox)

## Lo que hizo Claude Code

- REPL mode wraps primitive tools (Bash, Read, Write, Edit, Glob, Grep, NotebookEdit, Agent) — hides them from model, forces batch/scripted operations through a single REPL tool
  → `claude-code-leak/src/tools/REPLTool/constants.ts:37-46`
- REPL is a "transparent wrapper" — no UI of its own, inner tool calls render natively
  → `claude-code-leak/src/Tool.ts:529-533`
- REPL is ant-only, gated on `USER_TYPE === 'ant'` + `CLAUDE_CODE_ENTRYPOINT === 'cli'`
  → `claude-code-leak/src/tools/REPLTool/constants.ts:23-30`
- Main REPLTool handler source NOT in leak — compiled JS only. This is a proprietary internal tool.
  → `claude-code-leak/src/tools.ts:16-19`
- Sandbox: bubblewrap (Linux) / sandbox-exec (macOS) via `@anthropic-ai/sandbox-runtime`
  → `claude-code-leak/src/utils/sandbox/sandbox-adapter.ts:172-381`
- Transcript invisibility for external users: REPL wrappers stripped from persisted transcripts
  → `claude-code-leak/src/utils/sessionStorage.ts:4396-4429`

## Lo que hizo OpenClaw

- No dedicated REPL tool. Uses `exec background=true usePty=true` to start interpreter, then `process` tool (write/send-keys/submit/paste) for interactive I/O
  → `research/src/agents/bash-tools.process.ts:97-617`
- Two-tier process management: ProcessSupervisor (spawn/cancel/timeout) + ProcessSession registry (aggregated output, exit notifications)
  → `research/src/process/supervisor/supervisor.ts`
  → `research/src/agents/bash-process-registry.ts`
- PTY mode via node-pty for terminal emulation (xterm-256color, 120x30 default)
  → `research/src/process/supervisor/adapters/pty.ts`
- Sandbox: Docker-based (`docker exec` into persistent container, network disabled, workspace bind mounts)
  → `research/src/agents/bash-tools.exec-runtime.ts:643-667`
  → `research/docs/gateway/sandboxing.md`

## Lo que cortaron / limitaciones

- Claude Code REPL source is missing — we can't port the handler logic, only the concept
- Claude Code REPL is Claude-specific (relies on model understanding the wrapper abstraction)
- OpenClaw approach is Unix-only (PTY, Docker)
- Neither has a cross-platform, provider-agnostic REPL tool
- Neither integrates with a tool registry pattern like ours

## Ideas para el framework Rust

### 1. REPL as a stateful process tool (not a wrapper)
A tool that spawns Python/Node/bash REPL processes, keeps them alive across turns, and lets the LLM send code and read output. Provider-agnostic: any LLM can call `Repl { action: "exec", code: "..." }` or `Repl { action: "read" }`.

Ventaja sobre Claude Code: no depende de que el modelo entienda el patrón wrapper. Es un tool normal con JSON schema. Válido para Anthropic, OpenAI, Gemini, MiniMax, Generic.

### 2. ReplRuntime — persistent subprocess manager
A `ReplRuntime` struct that:
- Spawns interpreter process via `tokio::process::Command` with piped stdin/stdout/stderr
- Keeps process alive across tool calls (stored in `Arc<DashMap<String, ReplSession>>`)
- Session keyed by `session_id` so multiple REPLs can coexist
- PTY allocation optional (Linux only, via `tokio::process::Command` with `setsid()`)
- Output buffer with cap (last N KB) to prevent memory exhaustion
- Timeout + heartbeat to detect dead processes

Ventaja sobre OpenClaw: no depende de Docker. Proceso directo, más ligero, funciona en Termux/ARM.

### 3. Tool schema — action-based
```json
{
  "name": "Repl",
  "description": "Run code in a stateful Python/Node/bash REPL session. Sessions persist across turns.",
  "parameters": {
    "type": "object",
    "properties": {
      "action": {
        "type": "string",
        "enum": ["spawn", "exec", "read", "kill", "list"]
      },
      "session_id": { "type": "string", "description": "Session ID from spawn" },
      "runtime": { "type": "string", "enum": ["python", "node", "bash"] },
      "code": { "type": "string", "description": "Code to execute in the REPL" }
    },
    "required": ["action"]
  }
}
```

### 4. Security — config-gated, not hardcoded
- Feature flag: `repl-tool` (off by default — opt-in per binding)
- Config: `repl: { enabled: true, allowed_runtimes: ["python", "node"], max_sessions: 3, timeout_secs: 30, max_output_bytes: 65536 }`
- No sandbox in MVP — rely on OS process isolation. Sandbox (bubblewrap/Docker) deferred to Phase 79.12.b.
- Per-session filesystem access: `cwd` + optional `allowed_paths`

### 5. Integration pattern
- New file: `crates/core/src/agent/repl_tool.rs`
- `ReplRuntime` stored in `AgentContext` (or a new `ReplRegistry` behind `Arc`)
- Registration in `main.rs` gated on binding config `repl.enabled`
- Classification in `plan_mode.rs` — `MUTATING_TOOLS`
- Optional: expose via MCP as `repl_spawn`/`repl_exec`/`repl_read`/`repl_kill`

### 6. Provider-agnostic by design
- Tool uses standard JSON Schema — compatible with all LLM providers
- No Claude-specific system prompt modifications
- No wrapper/hiding of other tools
- Any provider that supports tool calling can use it

## Ideas descartadas

- **REPL as transparent wrapper (Claude Code pattern)** — Claude-specific, requires model to understand wrapper semantics, breaks provider-agnostic requirement
- **Docker sandbox mandatory (OpenClaw pattern)** — heavy dependency, doesn't work on Termux/ARM, overkill for MVP
- **Full PTY terminal emulation** — complex, platform-specific, deferred to follow-up phase
- **Remote REPL via bridge (Claude Code bridge pattern)** — out of scope, different use case (remote control, not code execution)
