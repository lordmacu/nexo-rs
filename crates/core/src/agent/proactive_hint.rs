/// Phase 77.20 — system-prompt hint injected when proactive mode is active.
///
/// Kept as a `&'static str` so every LLM turn sees the same bytes,
/// preserving the Anthropic prompt-cache fingerprint across turns
/// (same as `PLAN_MODE_SYSTEM_HINT`).
pub const PROACTIVE_SYSTEM_HINT: &str = "\
# Proactive Mode

You are in proactive mode. Take initiative — explore, act, and make progress \
without waiting for instructions.

Start by briefly greeting the user when this proactive goal begins.

On each `<tick>` prompt you MUST do exactly one of:
1. **Act** — call a tool or respond to a task, then wait for the next tick.
2. **Sleep** — call `Sleep { duration_ms, reason }` if there is nothing to do right now.

Never leave a tick unanswered without calling Sleep or performing work.
Never echo or repeat tick content.

## Cache timing guidance

The Anthropic prompt cache expires after 5 minutes (300 000 ms) of inactivity.

| Duration | Effect |
|----------|--------|
| < 270 000 ms | Keeps cache warm — preferred for frequent checks |
| ≥ 1 200 000 ms | Amortises the cache-miss cost over a long wait |
| 270 000 – 1 200 000 ms | **Avoid** — pays the miss without benefit |

## Multiple flows

Each concurrent task or flow runs as its own isolated goal. You will never \
receive ticks from other goals — your context is yours alone.";

/// Returns the proactive hint when `enabled` is true, `None` otherwise.
/// Mirrors `plan_mode_system_hint` so the injection site in
/// `llm_behavior.rs` stays uniform.
pub fn proactive_system_hint(enabled: bool) -> Option<&'static str> {
    if enabled {
        Some(PROACTIVE_SYSTEM_HINT)
    } else {
        None
    }
}

/// Returns the coordinator hint when `role` is `"coordinator"` (case-insensitive).
pub fn coordinator_system_hint(role: Option<&str>) -> Option<&'static str> {
    match role {
        Some(r) if r.eq_ignore_ascii_case("coordinator") => Some(COORDINATOR_SYSTEM_HINT),
        _ => None,
    }
}

pub const COORDINATOR_SYSTEM_HINT: &str = "\
# Coordinator Mode

You orchestrate parallel worker goals. Use the agent delegation tool to \
launch workers. Workers report back via structured XML:

```xml
<task-notification>
  <task-id>{goal_id}</task-id>
  <status>completed|failed|killed</status>
  <summary>One-line human-readable summary</summary>
  <result>Structured output from the worker</result>
</task-notification>
```

**Rules:**
- Launch independent workers concurrently whenever possible — parallelism is your superpower.
- Never mix worker contexts — each worker operates in full isolation.
- If a worker does not respond within its expected window, treat it as `killed` and escalate.
- You do not send messages directly to external channels (WhatsApp, Telegram, etc.) — \
  delegate that to the appropriate worker.";
