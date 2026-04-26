# Anonymous telemetry (opt-in)

Nexo can emit a weekly heartbeat with **anonymous, aggregated**
deployment shape so the project knows what configurations are
actually in production. The heartbeat is **disabled by default** —
nothing leaves your host until you explicitly opt in.

This page documents exactly what's sent, what isn't, and how to
inspect the payload before enabling it.

## What is sent

Every 7 days (drift-resistant — 7d ± 1h jitter), if telemetry is
enabled, Nexo POSTs a single JSON document to
`https://telemetry.lordmacu.dev/nexo` over HTTPS:

```json
{
  "schema_version": 1,
  "instance_id": "0fa3...",
  "version": "0.1.1",
  "rust_version": "1.80.1",
  "os": "linux",
  "arch": "aarch64",
  "uptime_days": 14,

  "agents": {
    "total": 3,
    "active_24h": 2
  },

  "channels": {
    "whatsapp": 1,
    "telegram": 1,
    "email": 0,
    "browser": 1
  },

  "llm_providers": [
    "minimax",
    "anthropic"
  ],

  "memory_backend": "sqlite-vec",

  "sessions": {
    "average_per_agent_24h": 12,
    "p95_per_agent_24h": 28
  },

  "extensions_loaded": 4,

  "broker_kind": "nats"
}
```

## What is **not** sent

- ❌ **Message content.** Not a single byte of any conversation,
  prompt, response, or tool call ever leaves the host.
- ❌ **Identifiers.** No phone numbers, email addresses, contact
  names, agent names, channel handles. The `instance_id` is a
  random UUID generated on first opt-in and stored in
  `~/.nexo/telemetry-id`; it can't be tied to anything except a
  rerun of the same install.
- ❌ **API keys / tokens / secrets.** None. The provider list is
  the literal string `"minimax"`, never the key.
- ❌ **IP addresses.** The receiving server (`telemetry.lordmacu.dev`)
  drops the source IP at ingress before the payload hits any
  database. The HTTP access log retains only the country code
  derived from a one-way hash of the IP, used solely to plot the
  geographic distribution gauge on the public dashboard.
- ❌ **Hostname.** Not in the payload. Not derived from anything
  in the payload.
- ❌ **Time of day.** The heartbeat is jittered so the timestamp
  doesn't reveal a pattern.

## Why opt in

It's the only honest signal the project has about what's
actually deployed. Without it, every roadmap discussion is
guessing. With it, prioritization improves: if 80% of opt-in
deployments use Anthropic + WhatsApp, then a regression on that
combo gets a hot-fix; a niche feature goes to maintenance mode.

The aggregate dashboard at
`https://lordmacu.github.io/nexo-rs/usage/` (published once
Phase 41 fully ships) shows everyone what everyone else is doing
in aggregate — same data the maintainers see.

## Enable / disable

```bash
# Show current state + what would be sent right now
nexo telemetry status

# Enable (writes to /etc/nexo-rs/telemetry.yaml or ~/.nexo/telemetry.yaml)
nexo telemetry enable

# Inspect exactly what tomorrow's heartbeat will contain
nexo telemetry preview

# Disable + remove the instance_id file
nexo telemetry disable
```

Hot-reload aware (Phase 18) — toggling doesn't require a daemon
restart. The runtime watches the telemetry config; the next
heartbeat tick respects whatever is currently on disk.

## First-launch banner

On first `nexo` boot in a fresh install, the daemon prints once
to the journal:

```
========================================================================
  nexo telemetry is DISABLED.
  Enabling it sends an anonymous, aggregated weekly heartbeat
  describing your deployment shape (channel mix, LLM provider mix,
  agent count). No message content, no identifiers, no API keys.
  Inspect the payload:        nexo telemetry preview
  Enable:                     nexo telemetry enable
  Read the full spec:         https://lordmacu.github.io/nexo-rs/ops/telemetry.html
========================================================================
```

Subsequent boots stay silent. Toggling on or off prints a
one-line confirmation.

## Server-side guarantees

The receiving endpoint at `telemetry.lordmacu.dev`:

1. Drops the source IP **at the load balancer**, before the
   request reaches any application code or log aggregator.
2. Stores the JSON document verbatim with no enrichment.
3. Aggregates documents per `instance_id` only to compute the
   `active_install_count` cardinality on the public dashboard.
4. Retains raw documents for **90 days**, then aggregates and
   deletes the originals.
5. **Does not** correlate documents across `instance_id` rotations
   — if you `nexo telemetry disable && nexo telemetry enable`,
   you become a fresh install in the dataset.

The server source code lives at
`https://github.com/lordmacu/nexo-telemetry-server` (deferred —
opens once Phase 41 finishes server side). Reproducible build,
verifiable signatures.

## Inspecting in transit

The HTTP request is plain HTTPS POST with the JSON payload above
as the body. Easy to mitm in a corp environment:

```bash
mitmproxy -p 8888 -s drop_telemetry.py &
NEXO_TELEMETRY_PROXY=http://127.0.0.1:8888 nexo telemetry preview
```

The runtime respects `HTTPS_PROXY` / `HTTP_PROXY` / standard
proxy env vars for the heartbeat HTTP client (it goes through
the same `reqwest` client every other Nexo egress uses).

## Disabling at the firewall

If you just want to make sure no telemetry can leave even if it
gets accidentally enabled:

```bash
sudo iptables -A OUTPUT -d telemetry.lordmacu.dev -j REJECT
```

The runtime will see a network error in its logs every 7 days
(rate-limited to once-per-week to not flood). It does not
retry-forever — one attempt per scheduled tick.

## Compliance notes

- **GDPR**: anonymous aggregate data with no identifiers and no
  PII falls outside Article 4(1) "personal data". The
  `instance_id` is technical metadata, not a pseudonym — it
  can't be re-tied to a natural person via any data the project
  holds.
- **HIPAA**: no PHI is collected; the field set is
  infrastructure metadata only.
- **Corporate sec teams**: the receiving endpoint speaks only
  HTTPS, no fallback to HTTP. The server cert is publicly
  pinnable. The payload schema is documented + versioned; new
  fields require bumping `schema_version` and a documented
  changelog entry below.

## Schema changelog

| Version | Released | What changed |
|---|---|---|
| 1 | TBD when Phase 41 ships | Initial schema as documented above |

Future schema changes append a row here. Old clients are not
forced to upgrade — the server accepts every advertised
`schema_version` indefinitely (rolled-up dashboard panels
include only the fields a given schema carries).

## Out of scope

- **Per-agent / per-binding metrics** — that's the Prometheus
  `/metrics` endpoint, scraped locally by your own Prometheus
  (see [Grafana dashboards](../ops/grafana/README.md)). The
  telemetry heartbeat is **deployment-shape** only.
- **Crash reports** — Nexo emits anyhow backtraces to the local
  journal but never sends them off-host.
- **Real-time analytics** — heartbeat is once weekly. There's no
  call-home for live metrics, ever.
