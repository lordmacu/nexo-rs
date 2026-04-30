# nexo-dream — Phase 80.1

Forked-subagent memory consolidation (KAIROS port). Verbatim semantics
from `claude-code-leak/src/services/autoDream/`.

## What it does

When a long-running goal accumulates ≥ 5 sessions of transcripts since
the last consolidation AND ≥ 24h have elapsed AND no other process is
mid-consolidation, AutoDream forks a subagent that runs a 4-phase
"Orient → Gather → Consolidate → Prune" prompt over the memory directory
to dedupe / merge / fix / prune memory files. The fork shares the
parent's prompt cache (via `nexo-fork::CacheSafeParams`) and is
restricted to read-only filesystem ops + Edit/Write inside the memory
directory (via `nexo-fork::AutoMemFilter`).

Audit rows persist via `nexo-agent-registry::DreamRunStore` so the run
survives daemon restart and is queryable post-mortem.

## Provider-agnostic

Built on `nexo_llm::LlmClient` trait. Works under Anthropic / OpenAI /
MiniMax / Gemini / DeepSeek without code change.

## Reference

- Verbatim port: `claude-code-leak/src/services/autoDream/autoDream.ts:1-324`
- Lock: `claude-code-leak/src/services/autoDream/consolidationLock.ts:1-140`
- Prompt: `claude-code-leak/src/services/autoDream/consolidationPrompt.ts:1-65`
- Watcher: `claude-code-leak/src/services/autoDream/autoDream.ts:281-313`
- Decisions: `proyecto/design-kairos-port.md` (D-1 coexistence with
  Phase 10.6 scoring sweep)

## Nexo extensions vs leak parity

The following are **nexo additions** (NOT in leak) — documented as
intentional divergence:

- `RunOutcome` enum — leak returns `Promise<void>`. Nexo exposes
  structured outcome for CLI / LLM-tool feedback.
- `dream_now` LLM tool + `nexo agent dream` CLI — leak's `isForced()`
  is hardcoded `false`. Nexo exposes the manual path.
- Buffer pattern `_pending_promotions.md` — leak only has fork pass.
  Nexo has both Phase 10.6 scoring sweep + 80.1 fork (D-1
  coexistence); buffer serializes the two writers.
- Post-fork `files_touched` escape audit — leak trusts
  `createAutoMemCanUseTool` only. Nexo defense-in-depth (3-pillar
  Robusto).
- 5-provider transversality tests — leak is Anthropic-only. Nexo
  memory rule (`feedback_provider_agnostic.md`) requires cross-provider
  verification.

The following match leak verbatim:

- Gate ordering (kairos / remote / memory / time / scan / sessions / lock).
- `MAX_TURNS = 30` (via 80.18) and 4-phase consolidation prompt.
- Lock `mtime IS lastConsolidatedAt`, PID body, `HOLDER_STALE = 1h`.
- `PER-TURN HOOK` (NOT cron) — invoked from driver-loop's per-turn
  loop alongside Phase 77.5 `extract_memories`.
- `tracing::info!` events with leak field names (`hours_since`,
  `sessions_since`, `cache_read`, `cache_created`, `output`,
  `sessions_reviewed`).
- No heartbeat lock during fork (leak doesn't have it; `holder_stale`
  defaults to 1h, which suffices for typical sub-1h forks).
