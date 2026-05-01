# Webhook receiver

Inbound HTTP webhook surface for any third-party provider that
signs payloads with HMAC-SHA256 / HMAC-SHA1 / a raw shared token
and exposes the event kind in a header or JSON body field.
Provider-agnostic by construction: declare sources in YAML, no
Rust code change per provider.

Successful requests are published to a NATS subject; downstream
pollers, agent turns, or microapps subscribe and react.

## Quick start

```yaml
# config/webhook_receiver.yaml
enabled: true
bind: "0.0.0.0:8081"
body_cap_bytes: 1048576
request_timeout_ms: 15000

# (optional) defense for floods — token-bucket per (source, ip).
default_rate_limit:
  rps: 10
  burst: 20

# (optional) max in-flight requests per source. 0 = unbounded.
default_concurrency_cap: 32

# (optional) honour `X-Forwarded-For` only when the socket peer
# is in one of these CIDR blocks.
trusted_proxies:
  - "10.0.0.0/8"
allow_realip_fallback: false

sources:
  - id: "github_main"
    path: "/webhooks/github"
    signature:
      algorithm: "hmac-sha256"
      header: "X-Hub-Signature-256"
      prefix: "sha256="
      secret_env: "WEBHOOK_GITHUB_MAIN_SECRET"
    publish_to: "webhook.github_main.${event_kind}"
    event_kind_from:
      kind: "header"
      name: "X-GitHub-Event"

    # (optional) per-source overrides
    rate_limit:
      rps: 20.0
      burst: 40
    concurrency_cap: 8
```

Set the secret in the environment before starting the daemon:

```bash
export WEBHOOK_GITHUB_MAIN_SECRET='your-shared-secret'
```

## Pipeline

Every accepted POST goes through six gates in order. Failure at
any gate short-circuits the request; the dispatcher only fires
when every gate passes.

| Gate | Reject status | What it checks |
|------|---------------|----------------|
| 1. Method | 405 | Only `POST <path>` matches the route. |
| 2. Body cap | 413 | `tower_http::limit::RequestBodyLimitLayer` enforces per-source `body_cap_bytes`. |
| 3. Concurrency | 503 + `Retry-After: 1` | Per-source semaphore. `0` = unbounded. |
| 4. Rate limit | 429 | Token bucket per `(source_id, client_ip)`. LRU-evicts at 4096 keys to defend against IP-flood OOM. |
| 5. Signature | 401 / 422 / 500 | HMAC verify (constant-time) + event-kind extract from header or JSON body path. `500` only when `secret_env` is unset. |
| 6. Dispatch | 502 / 422 | `BrokerWebhookDispatcher` publishes the envelope. `502` = broker unavailable; `422` = envelope serialise rejected. |

Successful dispatch returns `204 No Content`.

## NATS envelope

The dispatcher publishes a typed `WebhookEnvelope` (JSON):

```json
{
  "schema": 1,
  "source_id": "github_main",
  "event_kind": "pull_request",
  "body_json": { "action": "opened", "...": "..." },
  "headers_subset": {
    "x-github-delivery": "abc-123",
    "user-agent": "GitHub-Hookshot/..."
  },
  "received_at_ms": 1746147600000,
  "envelope_id": "0c4a...-uuid",
  "client_ip": "1.2.3.4"
}
```

Subscribers can filter on `topic == "webhook.<source_id>.<event_kind>"`
or on the broker `Event.source` field (which doubles as
`source_id`).

### Headers forwarded vs stripped

Forwarding every header would leak `Authorization` / `Cookie` /
the signature itself to NATS subscribers. The receiver allowlists
just the non-secret correlation headers downstream consumers
actually need:

- `x-github-delivery`
- `x-stripe-event-id`
- `x-event-id`
- `x-request-id`
- `idempotency-key`
- `user-agent`

## Operating behind a reverse proxy

If the daemon is behind nginx / Cloudflare / a load balancer:

1. Set `trusted_proxies` to the proxy's source CIDR.
2. Optionally enable `allow_realip_fallback` if your proxy uses
   `X-Real-IP` instead of `X-Forwarded-For`.

Untrusted peers always have their forwarded headers ignored —
clients claiming to be a proxy from outside the trusted CIDR
get their socket address used for rate-limit keying. This is the
correct defensive posture; tighten `trusted_proxies` until only
your real proxies fit.

## Reserved ports

- `8080` — health server (Kubernetes liveness)
- `9091` — admin server (loopback only)

The webhook bind address must not collide with either; validation
rejects collisions at boot with a typed
`WebhookConfigError::ReservedBind`.

## Secret rotation

Secrets are read fresh per request via `std::env::var` — no
caching. To rotate:

1. Set the new value in the environment.
2. Restart the daemon (env reads happen on every request, but
   the original env at start time wins; safest is restart).
3. Verify with a known-good signed request.

## Troubleshooting

- **All requests 401**: `tracing::warn!` shows `signature
  mismatch`. Re-check that the operator-side
  `WEBHOOK_<SOURCE>_SECRET` env matches what the provider signs
  with.
- **All requests 500**: `secret_env` is unset. Check the
  environment for the configured variable name.
- **Bursts get 429s**: tighten the provider's retry/backoff or
  raise `default_rate_limit.burst`. Token-bucket allows bursts up
  to `burst` then drops at `rps` — design for steady-state load
  + a margin.
- **Bursts get 503s**: `default_concurrency_cap` reached. Raise
  the cap, or lower the per-source `concurrency_cap` for noisy
  sources to keep them from starving the rest.

## Validation errors at boot

| Error | Cause | Fix |
|-------|-------|-----|
| `BodyCapZero` | `body_cap_bytes: 0` | Raise to a positive value (default 1 MiB). |
| `RequestTimeoutZero` | `request_timeout_ms: 0` | Raise to a positive value (default 15 000 ms). |
| `DuplicateId` | Two sources share an `id`. | Rename one. |
| `DuplicatePath` | Two sources share a `path`. | Pick distinct paths. |
| `ReservedBind` | `bind` port is 8080 or 9091. | Pick a free port. |
| `Source { id, detail }` | Per-source schema invalid. | Read `detail` — typically empty `path` or empty `secret_env`. |
| `DefaultRateLimit` | `rps` negative or > 1000. | Use a sane positive value. |
| `ConcurrencyCapZero` | Per-source `concurrency_cap: 0` | Use `null` to inherit the global cap. |
