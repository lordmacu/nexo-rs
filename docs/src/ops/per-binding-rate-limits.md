# Per-binding tool rate-limits

Phase 82.7 lets operators declare **per-binding** tool rate-limits
on top of the per-agent ones from Phase 9.2. Same agent + same
tool, two bindings → two independent buckets with independent
caps. Use it to enforce SaaS tier policies (free / pro /
enterprise) without spinning up separate agent processes.

## When to use

- Same agent answers a free-tier WhatsApp account AND an
  enterprise account; the enterprise tenant must not be starved
  by free-tier traffic on the shared `marketing_send_drip` tool.
- An event-subscriber binding ingests cron tickers — these
  should run unlimited regardless of how the agent's other
  bindings are configured.
- A `webhook` binding receives bursty `github` events; you want
  a cap so a runaway CI pipeline can't spam the LLM.

The agent-level `tool_rate_limits` from Phase 9.2 still applies
when no per-binding override is declared. When an override IS
declared on the matched binding, it FULLY REPLACES the global
decision for that binding (no fall-through to global patterns).

## Wire shape

```yaml
agents:
  - id: ana
    inbound_bindings:
      - plugin: whatsapp
        instance: free_tier
        tool_rate_limits:
          patterns:
            marketing_send_drip:
              rps: 0.167         # 10 per minute
              burst: 10
              essential_deny_on_miss: true
            "memory_*":
              rps: 1.0
              burst: 5
            _default:
              rps: 5.0
              burst: 20

      - plugin: whatsapp
        instance: enterprise
        # no override → unlimited (or global default if defined)

      - plugin: webhook
        instance: github
        tool_rate_limits:
          patterns:
            "*":                 # everything on this binding
              rps: 2.0
              burst: 10
```

### Field reference

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `patterns.<glob>.rps` | f64 | required | Tokens added per second. `0.167` ≈ 10/min. |
| `patterns.<glob>.burst` | u64 | `ceil(rps).max(1)` | Initial bucket capacity. Higher burst = more leniency for bursty workloads. |
| `patterns.<glob>.essential_deny_on_miss` | bool | `false` | When `true`, the bucket is fail-closed: if LRU pressure evicts the bucket and the key is reallocated, the next call denies once before allocating fresh. Use for paid / quota-bound tools where you'd rather drop a single call than risk leaking quota. |
| `patterns._default` | object | none | Reserved key matched when no explicit pattern catches the tool. Same shape as other entries. |

### Glob matching

Same minimal glob as the agent-level patterns:

- `*` alone matches anything.
- `foo*` matches strings starting with `foo`.
- `*bar` matches strings ending with `bar`.
- `foo*bar` matches strings starting `foo` and ending `bar`.

Patterns evaluate in deterministic alphabetical order; first
match wins. `_default` is always last.

## Per-binding fully replaces global

Important semantic — different from how `allowed_tools` /
`outbound_allowlist` overrides work in some other crates:

- Binding declares `tool_rate_limits: Some(map)` → ONLY the
  patterns in `map` apply. Tools that don't match any pattern in
  the override (and don't match `_default` either) become
  **unlimited** on that binding, regardless of any global
  agent-level config.
- Binding declares `tool_rate_limits: None` (or the field is
  omitted) → fall through to agent-level
  `agents.<id>.tool_rate_limits` from Phase 9.2.

Operators wanting "binding tighter, with global fallback for
tools the binding doesn't mention" must explicitly include
those global patterns in the binding map. The full-replace
semantic is documented this way to keep the resolution path
unambiguous and predictable in audit logs.

## Free / pro / enterprise example

```yaml
- id: ana
  inbound_bindings:
    # Free tier — strict caps on paid tools
    - plugin: whatsapp
      instance: free_tier
      tool_rate_limits:
        patterns:
          marketing_send_drip:
            rps: 0.167          # 10/min
            burst: 10
            essential_deny_on_miss: true
          web_search:
            rps: 0.083          # 5/min
            burst: 5
          _default:
            rps: 1.0
            burst: 5

    # Pro tier — relaxed caps
    - plugin: whatsapp
      instance: pro
      tool_rate_limits:
        patterns:
          marketing_send_drip:
            rps: 1.667          # 100/min
            burst: 100
          _default:
            rps: 10.0
            burst: 50

    # Enterprise — unlimited (no override)
    - plugin: whatsapp
      instance: enterprise
```

A single `marketing_send_drip` flood from `free_tier` cannot
deny calls on `pro` or `enterprise`; their buckets are
independent.

## Bucket lifecycle + LRU eviction

Buckets are allocated lazily — the first call for a given
`(agent, binding_id, tool)` triple allocates a `TokenBucket`.

Bucket cardinality is capped (default `10_000`); the cap fires
only when allocating a new bucket would push the count past the
limit. Eviction picks the stalest bucket by `last_touch` (a
monotonic counter stamped on every `try_acquire`). Steady-state
traffic amortises eviction cost to near zero.

When the evicted bucket's config had `essential_deny_on_miss =
true`, the key is stamped into a separate "recently evicted
essentials" set. The next call for that key consumes the entry
and denies once, then allocates a fresh bucket. This adapts the
fail-open + ESSENTIAL deny opt-in pattern from upstream
production agent CLIs to the LRU eviction context.

## Phase 72 audit log marker

Every denial emits a `tracing::info!` event with the canonical
marker:

```text
rate_limited:tool=<name>,binding=<id|none>,rps=<f64>
```

Example:

```text
rate_limited:tool=marketing_send_drip,binding=whatsapp:free_tier,rps=0.167
```

`binding=none` indicates a denial on the legacy single-tenant
path (delegation receive, heartbeat, pre-Phase-82.7 callers).

Operator audit pipelines parse this format for billing / SaaS
fair-use metrics. The format is wire-shape stable —
`format_rate_limit_hit` in `nexo-tool-meta` is the source of
truth.

## Hot-reload behaviour

Per-binding overrides participate in the existing Phase 18
config snapshot path. After a yaml reload:

- Existing buckets keep their state until naturally aged out by
  LRU.
- New buckets allocated post-reload use the new config.
- Worst case is a single turn of slack while the snapshot
  swap propagates.

For an immediate cold start of all buckets, restart the daemon.

## Admin RPC integration

The limiter exposes `drop_buckets_for_agent(agent: &str)` so the
admin RPC delete-agent path (Phase 82.10) can clear `(agent, *,
*)` cells when an operator removes an agent. Without this,
buckets would leak until LRU eviction.

## Observability

Useful tracing fields when investigating denials:

- `agent_id` — which agent ran the call
- `marker` — canonical `rate_limited:...` string (parse for binding/tool/rps)
- `tool` — tool name as the LLM saw it

Tracking metrics:

- `nexo_rate_limit_buckets_active` — total live buckets across
  all agents (TODO; not yet emitted as Prometheus)

## Limitations

- Bucket evictions during a sustained burst can briefly allow a
  burst's worth of extra calls before the new bucket settles.
  Use `essential_deny_on_miss: true` on tools where this is
  unacceptable.
- The marker's `rps=` field reflects the configured rate at the
  time of denial. After a hot-reload that changes the rate, the
  marker may show the old value for buckets that haven't been
  re-resolved yet.
- The `_default` pattern only applies within its own scope: a
  per-binding `_default` does not fall through to the global
  `_default`.

## See also

- [Rate limiting & retry (LLM provider)](../llm/retry.md) — different layer, applies to outbound LLM calls.
- [Sender rate limit](../agents/sender-rate-limit.md) — drop-at-intake guard, runs before this limiter.
- [Capability toggles](./capabilities.md) — env-var-driven feature toggles separate from per-binding policy.
