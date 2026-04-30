# Assistant mode

Assistant mode is a per-binding behavioural toggle that flips an
agent into a proactive posture: it can act on its own when no user
is in the chat, run long-lived background goals, coordinate with
peers, and summarise activity when the user re-connects after a
silence. Default is **disabled** — bindings without the block keep
their conventional request-response behaviour.

## Quick start

```yaml
# agents.yaml
agents:
  - id: kate
    workspace_git:
      enabled: true     # required for `auto_dream` git commits
    dreaming:
      enabled: true
      interval_secs: 86400
    assistant_mode:
      enabled: true
      # Operator override (optional). When omitted, the bundled
      # default text is appended to the system prompt.
      system_prompt_addendum: null
      # Auto-spawn teammates at boot. Wired in 80.15.b follow-up;
      # accepted at parse time so YAML doesn't need migration later.
      initial_team: []
    auto_approve: true   # see `auto-approve.md`
    away_summary:
      enabled: true
      threshold_hours: 4
      max_events: 50
```

Boot logging confirms wiring:

```
INFO boot.assistant agent=kate assistant_mode runner registered
```

## What changes when assistant mode is on

### 1. Proactive system prompt addendum

The binding's effective system prompt picks up an addendum that
nudges the agent toward proactive behaviour:

> You are running in assistant mode. Your default posture is
> proactive: when the user is away, you may use scheduled triggers
> (cron) and channel inbound to drive your own actions, including
> spawning teammates, calling tools to gather context, and waiting
> on external events. When you have something useful to report,
> surface it succinctly through the configured outbound channel;
> otherwise stay quiet rather than narrating idle time. Only block
> on user input when you genuinely need a decision they can supply.

Operator can override the text via `system_prompt_addendum: "..."`.
Empty strings are rejected — omit the field to use the default.

### 2. Boot-immutable flag

The `enabled` flag is captured at boot. Toggling requires a daemon
restart so a single turn never sees a half-flipped state. The
addendum *content* itself IS hot-reloadable through the Phase 18
config-watcher path — operators can iterate on the prompt text
without bouncing the daemon.

### 3. Curated auto-approve dial (companion feature)

`assistant_mode: true` is most useful paired with `auto_approve: true`
(see `auto-approve.md`). Without the dial, the agent hangs on every
tool call waiting for interactive approval — the proactive
posture dies the first time it tries to run `ls /tmp`. With the
dial, safe read-only / scoped-write tools auto-allow while
destructive Bash, writes outside workspace, and self-config-edit
tools always ask.

`nexo setup doctor` warns when these are misaligned (assistant_mode
on but auto_approve off — see 80.17.c follow-up for the audit).

### 4. Always-on lifecycle

Bindings in assistant mode typically pair with:

- **BG sessions** (`agent run --bg`) — long-lived goals that
  survive shell exit. See `cli/agent-bg.md`.
- **AWAY_SUMMARY** — re-connection digest after silence. See
  `away-summary.md`.
- **Multi-agent coordination** — `list_peers` + `send_to_peer`
  for in-process peer messaging. See `multi-agent-coordination.md`.
- **Heartbeat / cron** — for time-driven proactive triggers
  (existing Phase 7 + future Phase 80.2 jitter cluster).

## Reading the flag from code

Boot-time helpers resolve the configured value through a single
view:

```rust
use nexo_assistant::ResolvedAssistant;
use nexo_config::types::assistant::AssistantConfig;

// At boot, per binding:
let resolved = ResolvedAssistant::resolve(cfg.assistant_mode.as_ref());
// AgentContext.assistant: ResolvedAssistant — read by:
//   - llm_behavior (system prompt addendum injection)
//   - cron defaults (80.15.c follow-up)
//   - brief mode auto-on (80.15.d follow-up)
//   - dream context kairos signal (Phase 80.1)
//   - remote-control auto-tier (80.17.b.b follow-up)
```

## Status (Phase 80 cluster)

The assistant-mode cluster ships across multiple sub-phases. As of
the most recent Phase 80 sweep:

| Sub-phase | Feature | Status |
|-----------|---------|--------|
| 80.15 | `assistant_mode` flag + addendum + `ResolvedAssistant` | ✅ MVP |
| 80.10 | `SessionKind` enum + `agent run --bg` + `agent ps` | ✅ MVP |
| 80.16 | `agent attach` + `agent discover` (DB-only viewer) | ✅ MVP |
| 80.17 + 80.17.b | `auto_approve` dial + decorator | ✅ MVP |
| 80.14 | AWAY_SUMMARY digest helper | ✅ MVP |
| 80.11 | Agent inbox + `list_peers` / `send_to_peer` tools | ✅ MVP |
| 80.11.b | Receive side router + per-goal buffer + render | ✅ MVP |
| 80.1 cluster | `auto_dream` fork-style consolidation | ✅ MVP |
| 80.16.b | Live event streaming via NATS for attach | ⬜ |
| 80.2-80.6 | Cron jitter cluster | ⬜ |
| 80.8 | Brief mode + `SendUserMessage` tool | ⬜ |
| 80.9 | MCP channels routing (7-step gate) | ⬜ |
| 80.12 | Generic webhook receiver | ⬜ |
| 80.21 | Docs + admin-ui sweep | ✅ (this page) |

Each sub-phase ships its infrastructure standalone (testable in
isolation, opt-in). Wiring the whole cluster end-to-end requires
the operator to thread the deferred main.rs hookup snippets when
their daemon dirty state allows.

## See also

- [Auto-approve dial](./auto-approve.md)
- [AWAY_SUMMARY digest](./away-summary.md)
- [Multi-agent coordination](./multi-agent-coordination.md)
- [Background agents (`agent run --bg` / ps / attach)](../cli/agent-bg.md)
- [Dreaming](../soul/dreaming.md) — autoDream consolidation
- [Proactive mode](./proactive-mode.md) — Phase 77.20 tick loop
