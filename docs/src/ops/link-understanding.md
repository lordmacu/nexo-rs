# Link understanding

When a user message contains URLs, the runtime can fetch them, extract
the main text, and inject a `# LINK CONTEXT` block into the system
prompt for that turn. The agent stops saying "I can't see what's at
that link" and starts answering against the actual page content.

The feature is **off by default**. Opt in per agent (and optionally
override per binding).

## Per-agent config

```yaml
# config/agents.yaml
agents:
  - id: ana
    link_understanding:
      enabled: true              # default: false
      max_links_per_turn: 3      # cap URLs fetched per message
      max_bytes: 262144          # 256 KiB per response, streamed
      timeout_ms: 8000           # per-fetch HTTP timeout
      cache_ttl_secs: 600        # 0 disables cache
      deny_hosts:                # appended to built-in denylist
        - internal.corp
```

Built-in denylist (always applied, cannot be removed):
`localhost`, `127.0.0.1`, `::1`, `metadata.google.internal`,
`169.254.169.254`. Defense against SSRF to internal endpoints.

## Per-binding override

Per-binding `link_understanding` overrides the agent default. Useful
to disable on a noisy channel:

```yaml
agents:
  - id: ana
    link_understanding: { enabled: true }
    bindings:
      - inbound: plugin.inbound.whatsapp.*
        link_understanding: { enabled: false }   # narrow on WA
      - inbound: plugin.inbound.telegram.*
        # inherits agent default (enabled: true)
```

`null` / omitted = inherit. Any object = full replace.

## What gets injected

For each fetched URL, one bullet:

```
# LINK CONTEXT

- https://example.com/post — Title of the page
  First paragraphs of main text, collapsed to ~max_bytes characters,
  HTML stripped, scripts and styles dropped.
```

The block lands inside the system prompt for that turn only. Cache
hits skip the fetch but still render the block.

## Hard caps (cannot be raised by config)

| Cap                     | Value                              |
|-------------------------|------------------------------------|
| URL length              | 2048 chars                         |
| Redirect chain          | 5 hops                             |
| User-Agent              | `nexo-link-understanding/0.1`      |
| Response stream cutoff  | `max_bytes` (drops the rest)       |
| Newlines / control chars in extracted text | sanitised (prompt-injection guard) |

## Operations

- A single shared `LinkExtractor` (HTTP client + LRU cache, capacity
  256) is built at boot and reused by every agent runtime in the
  process.
- Cache is in-process only. Restarts cold.
- Telemetry exported on `/metrics`:
  - `nexo_link_understanding_fetch_total{result="ok|blocked|timeout|non_html|too_big|error"}`
    — counter, one increment per fetch attempt.
  - `nexo_link_understanding_cache_total{hit="true|false"}` — counter,
    incremented on every TTL-cached lookup so dashboards can compute
    hit-rate without instrumenting the agent loop.
  - `nexo_link_understanding_fetch_duration_ms` — histogram (single
    series, no labels). Only observed for attempts that actually
    issued an HTTP request — cache hits and host-blocked URLs skip it
    so latency percentiles reflect real network work.

## When to leave it off

- Agents talking to untrusted senders where the agent must not be
  pivoted into fetching attacker-controlled URLs.
- Channels with strict latency budgets — a fetch can add up to
  `timeout_ms` to the turn.
- Privacy-sensitive deployments where outbound HTTP from the agent
  host is not allowed.
