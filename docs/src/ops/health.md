# Health checks

Three layers of health probes for a Nexo deployment, each tuned
for a different consumer:

1. **`/health`** — liveness. Cheap (atomic flag check). HTTP 200
   means the process is up; doesn't guarantee it can serve work.
2. **`/ready`** — readiness. Expensive (verifies broker
   connection, agents loaded, snapshot warm). HTTP 200 means
   the runtime can accept inbound traffic. Use this for
   load-balancer health checks.
3. **`scripts/nexo-health.sh`** — operator + monitoring. JSON
   summary with counter snapshots. Bridge until
   `nexo doctor health` (Phase 44) ships.

## Liveness — `/health`

Returns HTTP 200 + `ok` body when the agent process is alive.
The runtime sets a `RUNNING` flag at startup and clears it on
graceful shutdown. **Does not** verify any subsystem — useful
for "is the daemon there at all" probes.

```bash
curl -fsSL http://127.0.0.1:8080/health
# ok
```

Kubernetes liveness probe:

```yaml
livenessProbe:
  httpGet:
    path: /health
    port: 8080
  initialDelaySeconds: 30
  periodSeconds: 10
  timeoutSeconds: 3
  failureThreshold: 3
```

A failing liveness probe should restart the container. Be
generous on `initialDelaySeconds` — first-boot extension
discovery + memory open + agent runtime spin-up can take 15-25s.

## Readiness — `/ready`

Returns 200 only when **all** of:

- Broker (NATS or local) is reachable
- Every configured agent has loaded its tool registry
- The hot-reload snapshot has been warmed (Phase 18)
- Pairing store is open (if `pairing_policy.auto_challenge` is on)

Returns 503 with a JSON body listing the failing subsystem
otherwise:

```json
{
  "ready": false,
  "reasons": [
    {"subsystem": "broker", "detail": "nats://localhost:4222: connection refused"}
  ]
}
```

Use this for load-balancer / service-mesh routing decisions.
A node that's `live` but not `ready` should not receive
traffic.

```yaml
readinessProbe:
  httpGet:
    path: /ready
    port: 8080
  periodSeconds: 5
  timeoutSeconds: 2
  failureThreshold: 1
```

## Operator one-shot — `scripts/nexo-health.sh`

Single-shot JSON summary intended for `watch -n 5
nexo-health.sh` during ops, cron health-mailers, and uptime
monitors that want one structured payload covering everything.

```bash
# Default — pretty human output
scripts/nexo-health.sh

# JSON only (cron, monitoring scrapers)
scripts/nexo-health.sh --json

# Custom hosts (e.g., probing through a service mesh)
scripts/nexo-health.sh --host nexo.internal:8080 \
                      --metrics-host nexo.internal:9090

# Strict mode — open circuit breaker counts as unhealthy.
# Default mode tolerates breaker-open (degraded-but-up).
scripts/nexo-health.sh --strict
```

Pretty output:

```
============================================================
 nexo-rs health  ·  2026-04-26T15:30:00Z
============================================================

  overall:      ok
  admin:        127.0.0.1:8080
  metrics:      127.0.0.1:9090

  probes:
    ✓ live       ok
    ✓ ready      ok
    ✓ metrics    ok

  counters:
    tool_calls_total              4711
    llm_stream_chunks_total       28391
    web_search_breaker_open_total 0
```

JSON shape (for monitoring scrapers):

```json
{
  "overall": "ok",
  "timestamp": "2026-04-26T15:30:00Z",
  "endpoints": { "admin": "127.0.0.1:8080", "metrics": "127.0.0.1:9090" },
  "probes": [
    {"name": "live",    "status": "ok", "detail": "ok"},
    {"name": "ready",   "status": "ok", "detail": "{...}"},
    {"name": "metrics", "status": "ok", "detail": "# HELP nexo_..."}
  ],
  "counters": {
    "tool_calls_total":              4711,
    "llm_stream_chunks_total":       28391,
    "web_search_breaker_open_total": 0
  }
}
```

Exit codes:
- `0` — overall healthy
- `1` — at least one probe failed (or `--strict` and a breaker is open)

### Cron health mailer

```bash
# /etc/cron.d/nexo-health
*/5 * * * * nexo /opt/nexo-rs/scripts/nexo-health.sh --json --strict \
    >> /var/log/nexo-rs/health.jsonl 2>&1 \
    || (tail -1 /var/log/nexo-rs/health.jsonl | mail -s "nexo unhealthy" ops@yourorg)
```

Five-minute resolution, one line of JSONL per check, mail
on failure.

### Uptime monitor integration

UptimeRobot / BetterStack / Pingdom:

```
URL:        https://nexo.example.com/ready
Interval:   60s
Timeout:    5s
Expected:   HTTP 200
```

That's all most monitors need. The JSON body of `/ready`
explains the failure when the alert fires.

## What `nexo-health.sh` adds beyond `/ready`

| Signal | `/ready` | `nexo-health.sh` |
|---|---|---|
| Process up + accepting traffic | ✅ | ✅ |
| Counter snapshot (tool calls, LLM chunks) | ❌ | ✅ |
| Web-search breaker state | ❌ | ✅ |
| Single JSON payload | ❌ (HTTP 200/503) | ✅ |
| Suitable for HTTP probe | ✅ | ❌ (shells out) |

Use `/ready` for the orchestrator. Use `nexo-health.sh` for the
operator's eyeballs and the alerting pipeline.

## Status

Tracked as [Phase 44 — Auxiliary observability surfaces](https://github.com/lordmacu/nexo-rs/blob/main/proyecto/PHASES.md#phase-44).

| Capability | Status |
|---|---|
| `/health` liveness endpoint | ✅ shipped (Phase 9) |
| `/ready` readiness endpoint | ✅ shipped (Phase 9) |
| `scripts/nexo-health.sh` operator one-shot | ✅ shipped |
| Operator runbook (this page) | ✅ shipped |
| `nexo doctor health` aggregating subcommand | ⬜ deferred |
| `nexo inspect <session_id>` state-transition pretty-print | ⬜ deferred |
| Per-session structured event log under `data/events/` | ⬜ deferred |
