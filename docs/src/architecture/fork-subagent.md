# Fork subagent (Phase 80.19)

`crates/fork/` — fork-with-cache-share subagent infrastructure. A
**fork** is a lightweight in-process LLM turn loop that:

1. Shares the parent goal's prompt-cache key (system prompt, tools,
   model, message prefix) so cache hits transfer across the fork
   boundary.
2. Runs as a single LLM turn loop (`LlmClient::chat` + tool dispatch +
   loop), NOT through Phase 67's heavyweight goal-flow driver-loop
   (which spawns `claude` subprocesses and runs acceptance + workspace
   checks).
3. Optionally writes a transcript / agent-handle row, or stays
   invisible to `agent ps` when `skip_transcript: true`.

Fork is the primitive that consumes downstream sub-phases:

| Sub-phase | Use of fork |
|---|---|
| **80.1** autoDream consolidation | `ForkAndForget` + `AutoMemFilter` (80.20) + 4-phase prompt |
| **80.14** AWAY_SUMMARY | `ForkAndForget` + read-only memory whitelist + transcript scan |
| **Phase 51** eval harness | `Sync` mode + scripted prompts |
| Refactored `delegation_tool.rs` | `Sync` mode replacing the bespoke sync delegate |

The upstream `runForkedAgent` (`upstream agent CLI`)
is the verbatim reference. nexo's adaptation collapses 17 isolation
fields down to the handful that actually matter in Rust, because
`Arc<...>` shared state is already isolated by construction.

## Public surface

```rust
use nexo_fork::{
    DefaultForkSubagent, ForkSubagent, ForkParams, ForkOverrides,
    DelegateMode, QuerySource, CacheSafeParams, AllowAllFilter,
};

// 1. Snapshot the parent's last LLM request.
let cache_safe = CacheSafeParams::from_parent_request(&parent_chat_request);

// 2. Build a fork.
let handle = DefaultForkSubagent::new()
    .fork(ForkParams {
        parent_ctx,
        llm,
        tool_dispatcher,
        prompt_messages: vec![/* fork's first-turn user message */],
        cache_safe,
        tool_filter: Arc::new(AllowAllFilter),
        query_source: QuerySource::Custom("docs_example"),
        fork_label: "docs_example".into(),
        overrides: None,
        max_turns: 10,
        on_message: None,
        skip_transcript: true,
        mode: DelegateMode::ForkAndForget,
        timeout: Duration::from_secs(300),
        external_abort: None,
    })
    .await?;

// 3. Await completion when ready (or never, for true fire-and-forget).
let mut handle = handle;
let result = handle.take_completion().unwrap().await?;
```

## Cache-key invariant (CRITICAL)

`CacheSafeParams::fork_context_messages` MUST preserve any incomplete
`tool_use` blocks from the parent. Filtering them strips the paired
`tool_result` rows and breaks Anthropic's API (400 error), AND breaks
the cache prefix. nexo's `crates/llm` repairs missing pairings in
transport — same as the main thread — so identical post-repair prefix
keeps the cache hit.

Reference: `upstream agent CLI`.

```rust
// CORRECT — pass through unchanged
let cs = CacheSafeParams::from_parent_request(&req);

// WRONG — never do this
// cs.fork_context_messages.retain(|m| !has_dangling_tool_use(m));
```

The test
`cache_safe::tests::from_parent_request_preserves_message_prefix_with_partial_tool_use`
verifies bit-for-bit pass-through.

## Isolation strategy

KAIROS (TypeScript) clones 17 mutable fields per fork:
`readFileState`, `abortController`, `getAppState`, `setAppState`,
`setResponseLength`, `nestedMemoryAttachmentTriggers`,
`toolDecisions`, etc. Most of these are mutable closures or
mutable maps that JavaScript needs to deep-clone manually.

In nexo, every analogous field on `AgentContext` is either an `Arc`
(shared) or wrapped in `Arc<RwLock<...>>` (interior mutability with
explicit locking). Rust's ownership model already guarantees forks
cannot mutate the parent's state without going through the locks.

We therefore only override the fields whose isolation actually
matters:

| Field | Default | Override |
|---|---|---|
| `agent_id` | parent's value | `ForkOverrides::agent_id` |
| `critical_system_reminder` | none | `ForkOverrides::critical_system_reminder` (consumed by `run_turn_loop`) |
| `abort` | new child token; parent → child cascade only | `ForkParams::external_abort` (caller supplies) |
| `tool_filter` | `AllowAllFilter` | `ForkParams::tool_filter` (e.g. `AutoMemFilter` for 80.1) |

## DelegateMode

```rust
pub enum DelegateMode {
    Sync,            // block until completion
    ForkAndForget,   // tokio::spawn + return ForkHandle immediately
}
```

`ForkAndForget` is right when the caller (autoDream, AWAY_SUMMARY) does
not need the result inline. The handle's `Drop` impl cancels the
abort signal automatically when the future is never consumed —
prevents leaked tokio tasks if the handle is dropped without
`take_completion`.

## Telemetry

Every fork emits a `tracing` span `fork.subagent` with fields:

- `fork_run_id` — uuid v4
- `parent_agent` — `parent_ctx.agent_id`
- `fork_label` — caller-supplied tag (e.g. `auto_dream`)
- `query_source` — `QuerySource` variant
- `mode` — `Sync | ForkAndForget`
- `skip_transcript` — bool
- `cache_key_hash` — `u64` from `CacheSafeParams::cache_key_hash`

The turn loop additionally emits:

- `fork.cache_break_detected` (level WARN) when cache hit ratio
  drops below 0.5 on the first turn — actionable signal that the
  fork's `CacheSafeParams` does not match the parent. Phase 77.4
  cache-break heuristic.
- `fork.tool_filter` (level DEBUG) when the filter denies a tool call.

## AutoMemFilter (Phase 80.20)

`crates/fork::AutoMemFilter` is the canonical [`ToolFilter`] for
forked memory-consolidation work — autoDream (Phase 80.1),
AWAY_SUMMARY (Phase 80.14), eval harness (Phase 51 future). Verbatim
port of `upstream agent CLI`.

### What it allows

| Tool | Allowed when |
|---|---|
| `REPL` | always (inner primitives re-gate via this same filter; required for cache-key parity per upstream `:171-180`) |
| `FileRead`, `Glob`, `Grep` | always (inherently read-only) |
| `Bash` | `nexo_driver_permission::is_read_only(command)` — composes Phase 77.8 destructive-cmd warning + Phase 77.9 sed-in-place + a positive whitelist of ~45 read-only utilities + redirect / subshell / heredoc detection |
| `FileEdit`, `FileWrite` | `file_path` (post-canonicalize) starts with the filter's `memory_dir` |
| anything else | denied with structured `tool_result` body so the model can recover within the same turn |

### Defense in depth

1. **Whitelist allow-list** — only the seven tool names above; everything
   else is rejected at the filter layer.
2. **Bash classifier** — composes existing Phase 77.x classifiers + a
   conservative whitelist that intentionally drops `tee`, `awk`,
   `perl`, `python`, `node`, `ruby` because they can shell out via
   `system(...)`. Operators add them back per-call only if a
   pipe-only no-side-effects shape can be validated.
3. **Path canonicalize** at construction (`memory_dir` resolved once) AND
   per-call (`file_path` resolved before `starts_with`). Defeats
   symlink swaps and `..` traversal.
4. **Post-fork audit** in 80.1 — `auto_dream` independently re-checks
   `files_touched` paths after the fork completes, so a filter bypass
   would still be caught.

### Provider-agnostic

The filter operates on tool name + JSON args. It does NOT depend on
any specific [`LlmClient`] impl — works under Anthropic, OpenAI,
MiniMax, Gemini, DeepSeek, or any future provider that implements
the trait. Tool names are canonical nexo strings (`tool_filter::tool_names::*`);
provider clients translate to/from native wire formats.

The filter expects flat top-level args. If a provider client wraps
args in a nested envelope (e.g. `{"arguments": {...}}`), the client
MUST unwrap before dispatch — the filter denies nested shapes
explicitly so a missing unwrap surfaces immediately.

### Example

```rust
use std::sync::Arc;
use nexo_fork::{
    AutoMemFilter, DefaultForkSubagent, DelegateMode, ForkParams,
    ForkSubagent, QuerySource, CacheSafeParams,
};

let memory_dir = std::path::PathBuf::from("/var/lib/nexo/memory/agent_a");
std::fs::create_dir_all(&memory_dir)?;
let filter = Arc::new(AutoMemFilter::new(&memory_dir)?);

let handle = DefaultForkSubagent::new()
    .fork(ForkParams {
        parent_ctx,
        llm,
        tool_dispatcher,
        prompt_messages: vec![/* /dream prompt */],
        cache_safe: CacheSafeParams::from_parent_request(&parent_request),
        tool_filter: filter,           // ← whitelist applied here
        query_source: QuerySource::AutoDream,
        fork_label: "auto_dream".into(),
        overrides: None,
        max_turns: 30,
        on_message: None,
        skip_transcript: true,
        mode: DelegateMode::ForkAndForget,
        timeout: std::time::Duration::from_secs(300),
        external_abort: None,
    })
    .await?;
```

## Cross-process forks

Out of scope for 80.19. When Phase 32 multi-host orchestration lands,
a `NatsForkSubagent` impl will publish on
`agent.fork.<run_id>.events` so a fork can run on a remote daemon
sharing the parent's prompt cache via the upstream LLM provider's
cache plane.
