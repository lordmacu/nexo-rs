# Background agents — `agent run --bg` / `agent ps` / `agent attach` / `agent discover`

Operator-side CLI for spawning, listing, and inspecting goals that
should outlive the spawning shell. Pairs with [assistant mode](../agents/assistant-mode.md):
the agent runs in the background while you go do something else;
you check in via `agent ps` / `agent attach` whenever convenient.

## SessionKind

Every goal handle carries a `SessionKind` enum identifying how it
was spawned and how it should survive a daemon restart:

| Kind | Meaning | Survives restart |
|------|---------|------------------|
| `Interactive` | User-driven REPL turn or chat-channel inbound (default) | No — Phase 71 reattach flips `Running` → `LostOnRestart` |
| `Bg` | Operator spawned a detached goal via `agent run --bg` | Yes — keeps `Running` |
| `Daemon` | Persistent supervised goal (e.g. assistant_mode binding's always-on agent loop) | Yes |
| `DaemonWorker` | Worker spawned BY a `Daemon` goal — short-lived sub-agent | Yes (treated like `Bg` for reattach) |

Schema migration v5 adds the `kind` column to `agent_handles` SQLite
table; pre-80.10 rows default to `Interactive` automatically.

## `agent run [--bg] <prompt>`

Spawns a new goal handle. With `--bg`, sets `kind = Bg` + `phase_id =
"cli-bg"` + returns the goal_id immediately so the operator can
detach. Without `--bg`, sets `kind = Interactive` + `phase_id =
"cli-run"`.

```
$ nexo agent run --bg "review the latest commits and post a summary"
[agent-run] goal_id=a9f62654-688b-4e41-95c9-1ec2a1a39f6d
[agent-run] kind=bg
[agent-run] status=running (queued for daemon pickup)
[agent-run] prompt: review the latest commits and post a summary
[agent-run] detached — re-attach later with `nexo agent attach a9f62654-...`
```

Validates that the prompt is non-empty. JSON output via `--json`.

> **Note**: the slim MVP inserts the row into `agent_handles` but
> the **daemon-side pickup** of queued goals is deferred to
> 80.10.g. For now, the row sits `Running` until manually
> transitioned via `agent attach` (Phase 80.16) or a future
> supervisor.

## `agent ps [--all] [--kind=...] [--db=<path>] [--json]`

Reads `agent_handles.db` read-only and renders a markdown table.

```
$ nexo agent ps
# Agent runs (db: /home/.../state/agent_handles.db)

| ID       | Kind | Status  | Phase    | Started             | Ended |
|----------|------|---------|----------|---------------------|-------|
| a9f62654 | bg   | Running | cli-bg   | 2026-04-30T19:03:01 | -     |

1 rows shown.
```

Default filter: only `Running` goals. Use `--all` to include
terminal rows. Use `--kind=bg` (or `interactive` / `daemon` /
`daemon_worker`) to narrow.

JSON output for scripting:

```bash
$ nexo agent ps --json | jq '.[] | select(.kind == "bg")'
```

Empty / missing-DB case prints a friendly message and exits 0:

```
(no agent runs recorded yet — db not found at /home/.../agent_handles.db)
```

## `agent discover [--include-interactive] [--db=<path>] [--json]`

Operator's "what is running detached?" view. Filtered to `Bg` /
`Daemon` / `DaemonWorker` kinds by default. Pass
`--include-interactive` to broaden.

```
$ nexo agent discover
# Discoverable goals (db: /home/.../state/agent_handles.db)

| ID       | Kind | Phase  | Started             | Last activity        |
|----------|------|--------|---------------------|----------------------|
| a9f62654 | bg   | cli-bg | 2026-04-30T19:03:01 | 2026-04-30T19:25:42  |

1 goal(s).
```

Sort: `started_at` descending (newest first).

Empty result includes a hint:

```
(no detached / daemon goals running; pass --include-interactive to broaden)
```

## `agent attach <goal_id> [--db=<path>] [--json]`

Read-only viewer of a goal's latest persisted snapshot. Live
event streaming via NATS lands in 80.16.b — for now, the command
shows the most recent `AgentSnapshot` from the registry.

```
$ nexo agent attach a9f62654-688b-4e41-95c9-1ec2a1a39f6d
# Agent Goal a9f62654-688b-4e41-95c9-1ec2a1a39f6d

- **kind**: bg
- **status**: Running
- **phase_id**: cli-bg
- **started_at**: 2026-04-30 19:03:01 UTC

## Last progress
Reviewed last 12 commits, drafted summary, awaiting outbound channel hookup.

- **turn_index**: 4/30
- **last_event_at**: 2026-04-30 19:25:42 UTC

[attach] Live event stream requires daemon connection — re-run with NATS available (Phase 80.16.b follow-up).
```

For terminal goals, the hint changes:

```
[attach] Goal is in terminal state Done; no further updates expected.
```

Validates the UUID upfront (exit 1 with "is not a valid UUID");
exits 1 with "no agent handle found" when the row is absent.

## Database path resolution

All four commands resolve the `agent_handles.db` path the same
3-tier way (mirrors `agent dream` from Phase 80.1.d):

1. `--db <path>` (explicit override, beats everything)
2. `NEXO_STATE_ROOT` env → `<state_root>/agent_handles.db`
3. XDG default `~/.local/share/nexo/state/agent_handles.db`

The YAML tier is intentionally absent — `agents.state_root` is
not a config field today; state_root flows into `BootDeps`
directly per Phase 80.1.b.b.b documentation. Set
`NEXO_STATE_ROOT` to align the CLI with your daemon's actual
data dir.

## Reattach kind-aware

Phase 71 reattach (boot-time recovery) is now `SessionKind`-aware
since 80.10:

- `kind == Interactive` + `Running` pre-restart → flip to
  `LostOnRestart` (the user is gone; no caller waiting)
- `kind ∈ {Bg, Daemon, DaemonWorker}` + `Running` pre-restart →
  keep `Running` (the operator expects them to survive)

Use `reattach_running_kind_aware()` from
`SqliteAgentRegistryStore`. The legacy non-kind-aware
`reattach_running` stays for backward callers.

## Deferred follow-ups

- **80.10.b** = Phase 80.16 — `nexo agent attach` TTY re-attach
  (already shipped in DB-only viewer mode)
- **80.10.c** — Daemon supervisor process for `Daemon` /
  `DaemonWorker` kinds (separate process lifecycle distinct
  from the interactive daemon)
- **80.10.d** — `nexo agent kill <goal_id>` graceful abort signal
- **80.10.e** — `nexo agent logs <goal_id>` re-stream goal output
  without attaching
- **80.10.f** — Phase 77.17 schema-migration system integration
- **80.10.g** — Daemon-side pickup of queued goals: today the
  CLI inserts the row but no daemon worker consumes it
  automatically
- **80.16.b** — Live event streaming via NATS subscribe
  (`agent.registry.snapshot.<goal_id>` + `agent.driver.>`
  filtered by goal_id payload)
- **80.16.c** — User input piping via `agent.inbox.<goal_id>`
  (already wired in Phase 80.11 — multi-agent coordination
  uses the same channel; CLI input piping is the user-facing
  consumer)

## See also

- [Assistant mode](../agents/assistant-mode.md)
- [Auto-approve dial](../agents/auto-approve.md)
- [Multi-agent coordination](../agents/multi-agent-coordination.md)
- [AWAY_SUMMARY digest](../agents/away-summary.md)
