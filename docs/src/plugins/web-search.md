# Web Search plugin

Multi-provider web search (Brave / Tavily / DuckDuckGo /
Perplexity) for Nexo agents. Subprocess binary; daemon discovers
+ spawns via `[plugin.entrypoint]`.

> Phase 95 — extracted from `crates/web-search/` to standalone
> subprocess plugin
> [`nexo-rs-plugin-web-search`](https://github.com/lordmacu/nexo-rs-plugin-web-search)
> v0.1.0. Daemon's `web_search_router` field on
> `AgentContext` / `AgentRuntime` removed (`nexo-core 0.2.0`
> breaking).

## Install

```bash
cargo install nexo-plugin-web-search
```

The binary lands at `$HOME/.cargo/bin/nexo-plugin-web-search`.
Discovery walker probes it with `--print-manifest` and
auto-registers.

## Operator config

`<config_dir>/plugins/web-search.yaml`:

```yaml
instances:
  - id: default                     # required, unique
    # agent_id omitted → shared across all agents
    providers:
      brave:
        api_key_path: ./secrets/brave_api_key.txt
        timeout_ms:   8000
      tavily:
        api_key_path: ./secrets/tavily_api_key.txt
        timeout_ms:   10000
      duckduckgo:
        timeout_ms:   12000          # no API key required
    cache:
      enabled: true
      path:    ./data/web_search_cache.db
      ttl_secs: 3600
    default_order: [brave, tavily, duckduckgo]
```

### Multi-instance × multi-agent

Power-users with several agents each wanting their own search
profile declare multiple `instances:` entries. Optional
`agent_id` per instance scopes it to that single agent:

```yaml
instances:
  - id: default                     # shared baseline
    providers: { duckduckgo: {} }
    default_order: [duckduckgo]
  - id: research                    # private for ana
    agent_id: ana
    providers:
      perplexity:
        api_key_path: ./secrets/ana_perplexity.txt
    cache: { path: ./data/ana_research.db }
    default_order: [perplexity]
  - id: news                        # another private for ana
    agent_id: ana
    providers:
      brave:
        api_key_path: ./secrets/ana_brave.txt
    cache: { enabled: false }
    default_order: [brave]
```

Resolution per agent's `web_search` call:
1. `args.instance` if operator-supplied.
2. Agent's first private instance from `by_agent` map.
3. First shared instance (no `agent_id`).
4. Error if none.

## Tool surface

`web_search` arguments:

| Field | Required | Description |
|-------|----------|-------------|
| `query` | yes | Search query string. |
| `count` | no | 1-10; defaults from per-binding policy. |
| `instance` | no | Search profile id. Absent → agent's default. |
| `provider` | no | Provider override: `brave`/`tavily`/`duckduckgo`/`perplexity`. |
| `freshness` | no | Time window: `day`/`week`/`month`/`year`. |
| `country` | no | ISO-3166 alpha-2. |
| `language` | no | ISO-639-1. |
| `expand` | no | v0.1.0 no-op; v0.2.0 follow-up. |

Per-binding policy fields (`agents.yaml::inbound_bindings[].web_search`):

| Field | Default | Effect |
|-------|---------|--------|
| `enabled` | `false` | Gate. False blocks all `web_search` calls on this binding (returns Denied). |
| `provider` | `"auto"` | Default provider override. `args.provider` wins. |
| `default_count` | `5` | Default `count` when LLM omits it. |
| `cache_ttl_secs` | `600` | Per-router cache TTL hint. |
| `expand_default` | `false` | Default `expand` arg. |

## Admin RPCs

| Method | Params | Reply |
|--------|--------|-------|
| `nexo/admin/web_search/bot_info` | `{}` | plugin metadata + instance counts |
| `nexo/admin/web_search/cache_stats` | `{instance?}` | per-instance status |
| `nexo/admin/web_search/cache_clear` | `{instance?}` | placeholder (v0.2.0) |
| `nexo/admin/web_search/provider_status` | `{}` | per-instance configured providers |
| `nexo/admin/web_search/list_instances` | `{}` | full instances + by_agent + shared map |

## Metrics

Prometheus exposition format via broker scrape
`plugin.web_search.metrics.scrape`. Daemon's `/metrics`
aggregator appends.

## Source

[github.com/lordmacu/nexo-rs-plugin-web-search](https://github.com/lordmacu/nexo-rs-plugin-web-search)
— crates.io: `nexo-plugin-web-search 0.1.0`.
