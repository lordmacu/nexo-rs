# `RemoteTrigger` (Phase 79.8)

`RemoteTrigger` lets the model publish a JSON payload to a
**pre-configured** outbound destination — webhook (HTTP POST) or
NATS subject. Destinations live in the agent's YAML allowlist; the
model passes only `name` + `payload`, never URLs or subjects.

## Diff vs upstream

The upstream `RemoteTriggerTool`
(`upstream agent CLI`)
is a CRUD client for **claude.ai's hosted scheduled-agent API**
(`/v1/code/triggers`). Different concept entirely — Anthropic uses
"trigger" to mean "scheduled remote agent". Nexo-rs adopts the
*name* and ships a generic outbound publisher per our PHASES.md
spec. The two are conceptually unrelated; we cite the upstream CLI as
naming reference only.

## Configuration

```yaml
agents:
  - id: cody
    remote_triggers:
      - kind: webhook
        name: ops-pager
        url: https://hooks.example.com/abc
        secret_env: OPS_PAGER_SECRET   # optional — HMAC-SHA256 signs body
        timeout_ms: 5000               # default 5000
        rate_limit_per_minute: 10      # default 10; 0 = unlimited

      - kind: nats
        name: internal-ops
        subject: agent.outbound.ops
        rate_limit_per_minute: 30
```

Empty list (the default) keeps the tool registered but every call
refuses with `"no destination named X in this agent's allowlist"`.

## Tool shape

```json
{
  "name": "ops-pager",
  "payload": { "level": "warn", "msg": "build red on main" }
}
```

`payload` accepts any JSON shape (object / array / scalar). Cap is
**256 KiB serialised** — oversize is rejected before any network
call.

## Webhook headers

When dispatched as a webhook, every request carries:

| Header | Value |
|--------|-------|
| `Content-Type` | `application/json` |
| `X-Nexo-Trigger-Name` | trigger name (allowlist key) |
| `X-Nexo-Timestamp` | unix-seconds at dispatch |
| `X-Nexo-Signature` | `sha256=<hex>` HMAC of body using `secret_env` value (only when `secret_env` is set) |

Receivers MUST verify the signature when configured. Compute
`HMAC-SHA256(body, secret)` and compare against the
`X-Nexo-Signature` header in constant time.

## Rate limit

Sliding-window token bucket per trigger name, 1-minute window,
default 10 calls / minute. Set to `0` for unlimited (no bucket).
Bucket lives in process memory — restarts reset.

## Plan-mode

Classified `Outbound` (mutating) in
`nexo_core::plan_mode::MUTATING_TOOLS`. Plan-mode-on goals receive
`PlanModeRefusal` rather than a silent publish.

## Security model

1. **Allowlist.** The model sees only destination names; URLs and
   subjects are operator-owned in YAML. No way to coerce a
   trigger to a model-supplied URL.
2. **HMAC sign.** Optional but recommended. `secret_env` resolves
   at call time — secrets never enter YAML.
3. **Refuses unsigned when secret missing.** If
   `secret_env` is set but the env var is empty, the call refuses
   rather than send unsigned (defence in depth — shipping unsigned
   could bypass receiver auth).
4. **Body cap + rate limit.** Capacity controls bound the blast
   radius if a model goes haywire.
5. **Plan-mode gate.** A goal in plan mode cannot publish.

## Out of scope (deferred)

- **Per-binding override.** Today the canonical source is
  `agents[].remote_triggers`. A `binding.remote_triggers` override
  would let an operator scope per channel; not yet wired.
- **Circuit breaker per trigger.** Phase 2.5 `CircuitBreaker` is
  available but not yet wired in. Add when transient outbound
  failures become noisy enough to justify.
- **Telemetry counters.** `nexo_remote_trigger_calls_total{
  name, result}` + `nexo_remote_trigger_latency_ms{name}` are
  spec'd but not emitted. Wire when the tool is in active use.

## Diff vs upstream (summary)

| Aspect | upstream | Nexo-rs |
|--------|------|---------|
| Purpose | claude.ai CCR scheduled-agent CRUD | Generic outbound publisher |
| Auth | Anthropic OAuth | HMAC-SHA256 (operator-shared secret) |
| Destinations | hardcoded `/v1/code/triggers` | YAML allowlist (webhook / NATS) |
| Rate limit | Anthropic-side | Per-trigger token bucket in-process |

## References

- **PRIMARY**: PHASES.md::79.8 spec (own design).
  `upstream agent CLI`
  cited for naming + dispatcher shape only — semantics differ.
- **SECONDARY**: OpenClaw `research/` — no equivalent.
  Single-process TS reference uses plugin outbound paths
  directly; no allowlisted generic publisher exists.
