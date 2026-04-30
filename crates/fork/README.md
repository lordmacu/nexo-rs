# nexo-fork — Phase 80.19

Fork-with-cache-share subagent infrastructure. Standalone single-turn
loop using `nexo_llm::LlmClient` directly (NOT through Phase 67
driver-loop, which spawns `claude` subprocesses).

## What it provides

- `ForkSubagent` trait + `DefaultForkSubagent` impl.
- `CacheSafeParams` — fields that must match the parent's last LLM
  request for prompt-cache hit. Five-fold parity per Anthropic 4-block
  cache key (system / tools / messages / model).
- `ForkOverrides` — selective overrides applied to the parent
  `AgentContext` clone (most state stays shared via `Arc`).
- `DelegateMode { Sync | ForkAndForget }` — sync waits for completion;
  fire-and-forget returns a handle immediately (used by autoDream
  Phase 80.1, AWAY_SUMMARY 80.14, future Phase 51 eval harness).
- `ToolFilter` trait — per-fork tool whitelist (consumed by
  `AutoMemFilter` in 80.20).
- `OnMessage` trait — observation hook over the fork's message stream
  (used by 80.1's `FilesTouchedCollector`).
- `turn_loop::run_turn_loop` — standalone tool-dispatching turn loop.

## References

- Verbatim semantics from `claude-code-leak/src/utils/forkedAgent.ts`
  (lines 57-115, 131-145, 260-345, 345-462, 489-541, 75-83, 522-525).
- Decisions in `proyecto/design-kairos-port.md` D-8.
- Phase tracking in `proyecto/PHASES.md::Phase 80 — KAIROS`.

## Cache-key invariant

The leak (`forkedAgent.ts:522-525`) is explicit: do NOT
`filterIncompleteToolCalls` on `fork_context_messages`. Dropping
partial tool batches orphans paired results (API 400). Dangling
tool_uses are repaired downstream by `ensure_tool_result_pairing` in
`crates/llm` — same as the main thread — identical post-repair prefix
keeps the cache hit. Tests verify bit-for-bit pass-through.

## Out of scope

- Cross-process forks (Phase 32 multi-host orchestration).
- Subprocess-fork via `claude` binary (that lives in
  `crates/driver-loop` and is goal-flow heavyweight).
- The 17-field `ToolUseContext` clone semantics from leak — Rust's
  `Arc<...>` already isolates by construction; we only override the
  handful of fields whose isolation actually matters in nexo's
  architecture (abort signal + tool filter + agent_id + critical
  reminder).
