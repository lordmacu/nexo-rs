# Plugin supervisor (auto-respawn)

Subprocess plugins are isolated child processes. When one crashes,
the daemon supervisor can either pause + log (default) or
auto-respawn it with exponential backoff up to a bounded number of
attempts. This page documents the manifest knobs that control that
behaviour, the broker lifecycle events the supervisor publishes,
and the edge cases operators should plan for.

## Manifest knobs

```toml
[plugin.supervisor]
respawn = false              # opt-in. Default: false (Phase 81.21.b semantics)
max_attempts = 3             # cap on respawns before "gave_up". Default: 3
backoff_ms = 1000            # initial backoff; doubles per attempt, capped 60s. Default: 1000
stderr_tail_lines = 32       # ring buffer per running child for crash forensics. Default: 32
```

`respawn` is **opt-in** — community-tier plugins should not
silently keep restarting if they're broken. Operators that trust
their plugin (in-house adapters, well-tested community plugins)
flip the toggle on; everything else stays paused-on-crash.

`max_attempts` is the hard ceiling. After that many consecutive
respawn attempts the supervisor publishes `gave_up` and stops.
The operator must restart the daemon (or fix the plugin + redeploy)
to recover.

`backoff_ms` is the initial wait before the first retry. Each
subsequent attempt doubles the wait, capped at **60 seconds**.
Example with `backoff_ms = 1000`:

| Attempt | Wait |
|---|---|
| 1 | 1s |
| 2 | 2s |
| 3 | 4s |
| 4 | 8s |
| 5 | 16s |
| 6 | 32s |
| 7+ | 60s (capped) |

`stderr_tail_lines` is the per-running-plugin ring buffer of
recent stderr lines. On crash the supervisor drains it into the
`stderr_tail` field of the lifecycle events for forensic context.
Hard-capped at 512 by manifest validation.

## Lifecycle events (broker)

Every transition publishes a best-effort event on the daemon's
broker (NATS-style topic). Subscribers can stream these into
audit logs, dashboards, or alerts.

| Topic | When | Payload |
|---|---|---|
| `plugin.lifecycle.<id>.crashed` | Child exit detected (non-zero) | `{plugin_id, exit_code, stderr_tail: Vec<String>}` |
| `plugin.lifecycle.<id>.respawning` | Before each backoff sleep | `{plugin_id, attempt: u32 (1-indexed), backoff_ms: u64}` |
| `plugin.lifecycle.<id>.respawned` | After successful re-handshake | `{plugin_id, attempt, total_uptime_ms}` |
| `plugin.lifecycle.<id>.gave_up` | After `attempts >= max_attempts` | `{plugin_id, attempts, last_exit_code, stderr_tail}` |

`source` field on every event = `"plugin.supervisor"`.
`stderr_tail` is chronological (oldest first), capped at the
manifest's `stderr_tail_lines`.

`respawned.total_uptime_ms` is currently always `0` — real per-Inner
uptime telemetry is a deferred follow-up.

## Auto-respawn flow

```
Initial init() — spawn_one_attempt + handshake
                  │
                  ▼
            (child running)
                  │ ───── NormalExit (clean shutdown) ──── return
                  │
                  ▼ Crashed
       publish "crashed" event
                  │
                  │  ┌── respawn=false ──── return (Phase 81.21.b semantics)
                  │  │
                  │  ▼ respawn=true
       maybe reset attempt counter (heuristic)
                  │
                  │  ┌── attempt >= max_attempts ──── publish "gave_up" + return
                  │  │
                  │  ▼
       publish "respawning {attempt+1, backoff_ms}"
                  │
       sleep next_backoff(attempt) (or shutdown short-circuit)
                  │
       drain pending oneshots with "plugin restarted; retry"
                  │
       spawn_one_attempt + handshake
                  │
                  │  ┌── Err ──── attempt += 1; loop continues
                  │  │
                  │  ▼ Ok
       check shutdown_signaled (kill child if shutdown fired race)
                  │
       install new Inner; publish "respawned"
                  │
                  ▼
       attempt += 1; loop continues
```

## Reset attempt counter heuristic

If the most recent child sobreived ≥ `backoff_ms × max_attempts × 2`
milliseconds after a respawn, the supervisor treats the *next* crash
as a transient blip rather than a continuation of a respawn loop —
the attempt counter resets to 0. This permits recovery from network
blips / OAuth token refreshes / occasional segfaults without
masking real crash loops.

The window is hard-capped at `10 × 60s = 600s` so an over-tuned
manifest can't disable the heuristic entirely.

The window is **not** an operator knob; it derives from
`backoff_ms` + `max_attempts`. Operators that want a longer
window bump `backoff_ms` (which also slows down respawns) — that
trade-off is intentional. A future follow-up may expose
`restart_window_secs` as an explicit field if real-world demand
emerges.

## Shutdown semantics

- `shutdown()` flips a per-plugin atomic flag and notifies the
  supervisor immediately. A supervisor parked in backoff sleep
  wakes within milliseconds (no waiting up to 60s for the natural
  deadline).
- A shutdown that races a respawn handshake will kill the
  just-spawned child if shutdown fires between
  `spawn_one_attempt` returning Ok and the new `Inner`
  installation. No orphaned processes.
- The daemon-wide `ctx_shutdown` cancellation token is also
  observed. Either source returns the supervisor cleanly.

## Manual restart

Operators can force-restart any subprocess plugin from the admin
UI without restarting the daemon. Useful after a `gave_up` event
(auto-respawn loop exhausted) or to apply config changes that
only take effect at boot.

| Topic | Capability | Behaviour |
|---|---|---|
| `nexo/admin/plugins/restart { plugin_id }` | `plugin_restart` | Force-kill + fresh spawn + new respawn_loop |

The restart is **distinct** from auto-respawn:

- Publishes `plugin.lifecycle.<id>.restarted_manually` (NOT
  `crashed`+`respawned`) — operator dashboards can distinguish
  intentional restarts from crash recovery.
- Capability `plugin_restart` is separate from `plugin_doctor`
  (read-only). Security review can grant write+destructive
  separately from read access.
- Bypasses `respawn=false` — even with auto-respawn disabled, the
  manual restart spawns a fresh child + respawn_loop. After
  manual restart, the new respawn_loop respects the manifest's
  `respawn` setting again.

### Flow

```
operator clicks "Restart" in plugin admin UI
  ↓
RPC nexo/admin/plugins/restart { plugin_id }
  ↓
LivePluginRestarter.restart() — lookup + downcast + force_restart()
  ↓
SubprocessNexoPlugin::force_restart()
  ├─ capture previous_uptime_ms (Inner.spawned_at.elapsed())
  ├─ drain pending oneshots with "plugin restarted by operator"
  ├─ cancel.cancel() (cascade tears down writer/reader/forwarders/supervisor)
  ├─ wait up to 2s for supervisor task to drain
  ├─ force-kill child if still alive
  ├─ tokio::time::timeout(60s, spawn_one_attempt(...))
  ├─ capture new_pid from child.id()
  ├─ install new Inner
  ├─ spawn fresh respawn_loop
  ├─ publish "restarted_manually" event
  └─ return PluginsRestartResponse { plugin_id, previous_uptime_ms,
                                     restarted_at_ms, new_pid }
```

### Errors

| Error | Maps to | Operator action |
|---|---|---|
| `plugin {id} not found` | `InvalidParams` | Refresh admin UI; plugin removed from manifest |
| `plugin {id} is in-tree` | `InvalidParams` | Use daemon restart for in-tree plugins |
| `restart timed out` (60s) | `Internal` | Plugin in degraded state; inspect logs + fix manifest |
| `plugin handles not yet populated; daemon still booting` | `Internal` | Retry after 1-2s; daemon finishing `wire_plugin_registry` |

### Limitations

- **Subprocess plugins only** — in-tree plugins (`assistant`,
  `dispatch-tools`) cannot be hot-restarted. Operator restarts
  the daemon.
- **Manifest unchanged** — force_restart uses the cached manifest;
  operator-edited `manifest.entrypoint.command` won't take effect
  until daemon restart. Manifest hot-reload is a deferred
  follow-up.
- **No coalesce** — concurrent restart calls (two operators clicking
  simultaneously) execute sequentially via `self.inner.lock()`.
  Functional but with funny intermediate state for ~1s. Add
  explicit coalesce only if abuse seen.
- **No restart cooldown / rate-limiting** — capability gate is
  the gate. Add cooldown only if abuse seen.

## Limitations + open follow-ups

- **No manual restart RPC** — `nexo/admin/plugins/restart {id}`
  is a deferred follow-up. Today operators restart the daemon
  to recover from `gave_up`.
- **Real uptime telemetry** — `respawned.total_uptime_ms`
  always 0. Operators wanting per-cycle uptime should subscribe
  to `crashed` + `respawned` events and diff timestamps.
- **No Prometheus counter** — `nexo_plugin_respawn_total{plugin_id, outcome}`
  pending the general metrics pipeline.
- **No multi-recipient encrypt for stderr_tail** — captured
  plaintext only. A plugin that prints secrets to stderr will
  leak them via lifecycle events.
- **Per-attempt timeout** is the same `NEXO_PLUGIN_INIT_TIMEOUT_MS`
  used by the initial spawn. A respawn handshake that hangs
  beyond the timeout counts as a failed attempt.

## Operator checklist

1. Decide `respawn` per-plugin. Default `false` is safer; flip
   on for plugins you trust.
2. Tune `backoff_ms` to your plugin's recovery character. OAuth
   refresh blips: 1-5s. Network outages: 5-30s. Heavy boot
   plugins: 5s+ to avoid wasting CPU on tight retry loops.
3. Subscribe to `plugin.lifecycle.>` from a downstream system
   (audit log, alerting). The `gave_up` topic is the operator's
   clearest signal that human action is needed.
4. Read `stderr_tail` on `crashed` events for a quick crash
   triage before tailing log files manually.
