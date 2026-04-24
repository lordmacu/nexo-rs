# Rate limiting & retry

Every LLM provider client sits behind a token bucket and a bounded
retry policy with **decorrelated jittered exponential backoff**. This
page is the definitive reference for those two mechanisms.

Source: `crates/llm/src/retry.rs`, `crates/llm/src/rate_limiter.rs`,
`crates/llm/src/quota_tracker.rs`.

## Rate limiter

Token bucket, acquired before every outbound request.

- `interval = 1 / requests_per_second`
- One token per request
- Bucket fully refills after `interval` per slot
- **Per-provider, per-agent** — each client has its own bucket, so one
  noisy agent can't starve another even when they share a provider

```yaml
rate_limit:
  requests_per_second: 2.0
  quota_alert_threshold: 100000   # optional
```

At `2.0` rps, the bucket tops up a slot every 500 ms. A burst of 3
requests will wait briefly on the third.

### Quota tracker

Optional. When a provider returns remaining-quota info (header,
response body), `quota_tracker` records it via `record_usage()` on the
token response. If the remaining crosses `quota_alert_threshold`, a
structured `warn` log is emitted:

```
WARN quota threshold crossed  provider=minimax remaining=99500 threshold=100000
```

Pair with a Prometheus log-scraping rule for an alert.

## Retry policy

Retries live above the circuit breaker. They handle transient
failures that don't warrant flipping the breaker.

| Error class | Max attempts | Backoff curve |
|-------------|:-----------:|---------------|
| 429 (rate limit) | **5** | `max(retry-after, jittered_backoff)` |
| 5xx (server) | **3** | `jittered_backoff` |
| 401 (auth) | **1 refresh + 1 retry** | (internal to the client) |
| Other 4xx | **0** (fail fast) | — |

### Decorrelated jittered backoff

Not simple exponential — the next backoff is a uniform random draw in
a growing range:

```
next = uniform(base, max(base, last × multiplier))
```

Defaults from `llm.yaml` retry block:

| Field | Default |
|-------|---------|
| `initial_backoff_ms` | 1000 |
| `max_backoff_ms` | 60000 |
| `backoff_multiplier` | 2.0 |

Why decorrelated jitter: multiple clients hitting the same 429 don't
re-fire in lockstep. Desynchronization is built-in.

```mermaid
flowchart LR
    REQ[request] --> API{API response}
    API -->|200| OK[return ChatResponse]
    API -->|429| RL[RateLimit]
    API -->|5xx| SE[ServerError]
    API -->|401| AU[CredentialInvalid]
    API -->|4xx| F[Other fail fast]

    RL --> D1{attempts<br/>< 5?}
    SE --> D2{attempts<br/>< 3?}
    AU --> REF[auth refresh<br/>+ single retry]
    D1 -->|yes| BO1[wait max(retry_after,<br/>jittered_backoff)]
    D1 -->|no| F
    D2 -->|yes| BO2[wait jittered_backoff]
    D2 -->|no| F
    BO1 --> REQ
    BO2 --> REQ
    REF --> REQ
```

## Error classification per provider

The providers classify HTTP responses into a shared `LlmError` so the
retry layer can be common code:

| HTTP | `LlmError` variant | Retried? |
|------|--------------------|:--------:|
| 200 | `Ok(ChatResponse)` | — |
| 429 | `RateLimit { retry_after_ms }` | ✅ up to 5 |
| 5xx | `ServerError { status, body }` | ✅ up to 3 |
| 401 / 403 | `CredentialInvalid` | ❌ (client handles refresh internally) |
| Other 4xx | `Other` | ❌ |

## Tuning

- **Bursty workloads:** bump `requests_per_second` cautiously; the
  upstream's own rate limits won't move, so you'll just pay more 429s
  to find the ceiling.
- **Flaky networks:** raise `max_attempts` for 5xx; keep `max_backoff_ms`
  bounded so slow agents don't spiral.
- **Subscription plans:** lower `requests_per_second` to keep daily
  usage under caps; pair with `quota_alert_threshold`.

## See also

- [Fault tolerance — CircuitBreaker](../architecture/fault-tolerance.md#circuitbreaker)
- [Operations — Metrics](../ops/metrics.md)
