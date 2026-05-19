# Poller · RSS / Atom

Cron-style poller that sweeps RSS / Atom feeds and dispatches new
items to an agent as messages. Subprocess plugin; daemon spawns
it per tick under the Phase 96 poller v2 contract.

Source: standalone repo at
[`nexo-rs-poller-rss`](https://github.com/lordmacu/nexo-rs-poller-rss).

## Install

Two paths — pick whichever suits your operator workflow:

```bash
# Precompiled tarball (release.yml ships 5 targets — Linux musl x2,
# macOS x2, Windows MSVC). Drops under plugins.discovery.search_paths.
nexo plugin install lordmacu/nexo-rs-poller-rss

# Or compile from crates.io source.
cargo install nexo-poller-rss
```

Both paths land the binary as `nexo-poller-rss`; the daemon's
[`PluginPollerRouter`](../architecture/poller-v2.md) discovers
it via the `[plugin.poller]` manifest section.

## Configure

Minimal YAML — drop under `config/pollers.yaml`:

```yaml
- id: rss-tech-news
  kind: rss
  schedule: "*/15 * * * *"   # every 15 min
  target_agent_id: editor
  config:
    feeds:
      - https://example.com/blog.rss
    filters:
      title_contains: ["rust", "agent"]
```

Every matching item the poller hasn't seen before (cursor-driven
dedupe — see [Poller v2 architecture](../architecture/poller-v2.md))
dispatches as an agent prompt with title + url + body excerpt.

## Wire shape

Poller plugins implement the
[`PollerHandler`](https://docs.rs/nexo-microapp-sdk/latest/nexo_microapp_sdk/poller/trait.PollerHandler.html)
trait. The daemon hands the plugin a `TickRequest`
(kind + cursor + config + interval hint); the plugin returns a
`TickReply` (next_cursor + items_seen/items_dispatched metrics).

## See also

- [Poller v2 architecture](../architecture/poller-v2.md) — full
  spec of the `PollerHost` egress + `PluginPollerRouter` registry
- [Build a poller plugin (V2)](../recipes/poller-plugin.md) — how
  to write your own
- [Poller config reference](../config/pollers.md) — full YAML schema
