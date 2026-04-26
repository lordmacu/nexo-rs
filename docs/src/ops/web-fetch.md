# Web fetch

The `web_fetch` built-in tool lets an agent retrieve the cleaned
body text + title for one or more URLs the agent already knows.
Companion to [Web search](./web-search.md): `web_search` finds
URLs, `web_fetch` retrieves them.

Distinct from `web_search.expand=true` because the agent often
knows the URL up-front (skill output, RSS poll, calendar
attachment, user message) and would otherwise have to either
hallucinate a search query or shell out to a `fetch-url`
extension.

## When to use which

| Scenario | Tool |
|---|---|
| Agent needs to find content matching a query | `web_search` |
| Agent has a URL from a `web_search` hit and wants the body | `web_search(expand=true)` |
| Agent has a URL from a poller / skill / user message | `web_fetch` |
| Agent has a list of URLs to triage | `web_fetch(urls=[...])` |

## Tool signature

```jsonc
{
  "name": "web_fetch",
  "parameters": {
    "urls":      ["https://example.com/article", "https://other.com/page"],
    "max_bytes":  65536          // optional; clamped to deployment cap
  }
}
```

Response shape:

```jsonc
{
  "results": [
    {
      "url":   "https://example.com/article",
      "title": "Example article",
      "body":  "First paragraph...",
      "ok":    true
    },
    {
      "url":    "https://internal.intranet.local/private",
      "ok":     false,
      "reason": "fetch failed (host blocked, timeout, non-HTML, oversized, or transport error). Check `nexo_link_understanding_fetch_total{result}` for the bucket."
    }
  ],
  "count": 2
}
```

A bad URL returns a `{ok: false, reason}` row instead of bailing
the whole call, so the agent can still consume the successful
ones. Per-call cap of **5 URLs**; longer lists get trimmed with a
warn log.

## Configuration

`web_fetch` has no dedicated config. It rides on
[Link understanding](./link-understanding.md):

- `link_understanding.enabled` — gates the tool entirely. With
  it `false`, every fetch returns
  `{ok: false, reason: "disabled by policy"}`.
- `link_understanding.max_bytes` — deployment-wide ceiling. The
  tool's `max_bytes` arg can shrink but never grow past this.
- `link_understanding.deny_hosts` — host blocklist (loopback,
  private subnets, internal cloud metadata endpoints, plus
  whatever the operator added).
- `link_understanding.timeout_ms` — per-fetch HTTP timeout.
- `link_understanding.cache_ttl_secs` — cache TTL. Successful
  fetches are cached so a second `web_fetch` of the same URL
  inside the TTL is free.

Per-binding overrides via `EffectiveBindingPolicy::link_understanding`
(see [Per-binding capability override](../config/agents.md)).

## Telemetry

`web_fetch` reuses every counter the auto-link pipeline emits.
There's no separate dashboard:

- `nexo_link_understanding_fetch_total{result}` — `ok` /
  `blocked` / `timeout` / `non_html` / `too_big` / `error`.
- `nexo_link_understanding_cache_total{hit}` — `true` / `false`.
- `nexo_link_understanding_fetch_duration_ms` — histogram, only
  populated when an HTTP request actually went out (cache hits
  and host-blocked URLs skip it so percentiles reflect real
  fetch work).

The bundled Grafana dashboard
([`ops/grafana/nexo-llm.json`](https://github.com/lordmacu/nexo-rs/blob/main/ops/grafana/nexo-llm.json))
already plots all three.

## Why a per-call cap of 5 URLs

A runaway agent given the prompt "fetch every link in this 10k
RSS dump" would otherwise queue thousands of HTTP requests
synchronously, blowing the prompt budget and hammering the
target hosts. 5 covers every realistic agentic workflow
(read 3 candidates, pick the best two, summarise) while leaving
a clear ceiling. Operators who want batch behaviour should
spawn a [TaskFlow](../taskflow/model.md) that calls `web_fetch`
in chunks with cursor persistence.

## Comparison to extensions

The `fetch-url` Python extension does roughly the same thing.
`web_fetch` differs in three ways:

1. **In-process** — no subprocess spawn, no Python interpreter,
   no extension wire protocol. Sub-100ms cold path on the
   happy case.
2. **Shared cache + telemetry** — links the user shares (auto-
   expanded by Phase 21 link-understanding) AND links the
   agent fetches via `web_fetch` populate the same LRU. The
   second access is always free.
3. **Same security defaults** — same deny-host list, same size
   cap, same timeout. Operators tune one knob, two surfaces
   honour it.

Use the extension when the runtime path is wrong shape (custom
auth, post-only endpoints, non-HTML responses you want raw).
Use `web_fetch` for the standard "give me the article" case,
which is most of them.

## Implementation

The tool lives at
`crates/core/src/agent/web_fetch_tool.rs::WebFetchTool` and is
registered for every agent unconditionally in `src/main.rs`.
The per-binding `link_understanding.enabled` policy gates
whether the underlying fetch happens; the tool itself is always
visible in the agent's tool list so operators can write
"call web_fetch on URL X" prompts without needing a per-agent
`web_fetch.enabled` flag.

Source of truth for FOLLOWUPS W-2 closure.
