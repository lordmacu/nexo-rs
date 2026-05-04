# Plugin authoring overview

Phase 31.9. Entry point for authors building anything that
extends nexo-rs from the outside. This page gets you to the
right deeper guide in 60 seconds.

## Read this when

- You want to add capability to nexo-rs and have not yet picked
  between a plugin, an extension, or a microapp.
- You have picked "plugin" and need to know which language SDK
  to start with.
- You want a 5-minute end-to-end smoke test before committing
  to a language choice.

## Plugin vs Extension vs Microapp

nexo-rs ships three extension surfaces. They differ in who
owns the runtime, who owns the UI, and how operators install
them.

| You're building | Use | Owns UI? | Owns auth/billing? | Common languages |
|-----------------|-----|----------|--------------------|------------------|
| New channel (Slack, Discord, IRC) or poller | **Plugin** | No (daemon owns I/O) | No (operator config) | Rust, Python, TypeScript, PHP |
| Bundle of skills, advisors, prompts, or YAML config that operators `nexo ext install` | **Extension** | No | No | YAML + small Rust stubs |
| End-product on top of nexo-rs (multi-tenant SaaS, internal tool, white-label deploy) | **Microapp** | ✅ yes | ✅ yes | Any language with a NATS client |

If you are still unsure:

- **Plugin** if your code is reactive (`broker.event` fires →
  you do something) and ships as a binary the daemon spawns.
- **Extension** if your code is declarative (skills + agents +
  prompts) and ships as a tarball operators install with
  `nexo ext install`.
- **Microapp** if your code is the product. End users see your
  UI, your domain, your billing — nexo-rs is invisible
  infrastructure.

This page covers **plugins**. For extensions, jump to
[Manifest reference](../extensions/manifest.md). For
microapps, jump to
[Microapps · getting started](../microapps/getting-started.md).

## Pick a language

All four SDKs implement the same wire contract — your choice
is purely about ergonomics. Operators don't care which SDK you
picked; they just run `nexo plugin install <owner>/<repo>`.

| Language | Best for | Runtime deps | Per-target binaries? | SDK reference |
|----------|----------|--------------|----------------------|---------------|
| **Rust** | Performance, single static binary, zero runtime deps. | None — `cargo build` produces a static ELF/Mach-O. | ✅ yes (one tarball per Rust target) | [Rust SDK](./rust-sdk.md) |
| **Python** | Existing scripts, ML ecosystem, fast iteration. | `python3.11+` on operator host. | No (`noarch` — single tarball) | [Python SDK](./python-sdk.md) |
| **TypeScript** | Existing Node servers, npm ecosystem, frontend devs. | `node 20+` on operator host. | No (`noarch`) | [TypeScript SDK](./typescript-sdk.md) |
| **PHP** | Existing Composer / Symfony / Laravel codebase. | `php 8.1+` (Fibers required) on operator host. | No (`noarch`) | [PHP SDK](./php-sdk.md) |

Cross-cutting reference: [Plugin contract](./contract.md) is
the wire spec all four SDKs implement. Read it once and you
understand every SDK.

## 5-min quickstart

The shortest path from zero to a running plugin uses Rust
because the toolchain ships with cargo. Adapt the
`nexo plugin new --lang <other>` step for Python / TypeScript
/ PHP — the rest is identical.

```bash
# 1. Scaffold from the bundled template (Phase 31.6).
nexo plugin new my_plugin --lang rust --owner alice
cd my_plugin

# 2. Build (under a second on a warm cache).
cargo build

# 3. Boot the daemon with this directory injected at the head
#    of plugins.discovery.search_paths. No install, no verify,
#    no GitHub round-trip — pure inner-loop dev.
nexo plugin run .
```

Expected stderr trace from step 3:

```
INFO local plugin override applied (plugin_id=my_plugin)
INFO subprocess plugin spawned (id=my_plugin, pid=...)
INFO my_plugin starting
INFO subprocess plugin handshake ok (id=my_plugin, version=0.1.0)
```

The plugin is now live. Publishing any event on a topic the
plugin's manifest registers (default
`plugin.inbound.my_plugin_echo`) reaches the handler in
`src/main.rs::handle_event`.

To exit, send `Ctrl+C` — the daemon issues a `shutdown`
request, the plugin's `on_shutdown` runs, and both processes
return cleanly.

## Plugin config dir

Phase 81.4 — operators place per-plugin YAML config under
`<config_dir>/plugins/<plugin_id>/`. The daemon reads every
`*.yaml` / `*.yml` file in that directory at boot, deep-merges
them alphabetically, resolves `${ENV_VAR}` placeholders, and
(when your manifest declares a `schema_path`) validates the
merged tree against your JSONSchema before calling
`init()`. Validation failure aborts plugin load with
`InitOutcome::Failed`; the daemon continues without the plugin.

Multi-file sharding lets operators split sensitive settings
from declarative ones:

```text
<config_dir>/plugins/slack/
  01-credentials.yaml   # api_token: "${SLACK_BOT_TOKEN}"
  02-channels.yaml      # channels: [...]
  03-allowlist.yaml     # rate limits per channel
```

Mappings deep-merge across files (later wins per-key).
**Arrays full-replace** — they don't concat — so an operator
override file completely substitutes the array from earlier
files. Comment-only and non-`.yaml` files are ignored.

Declare your config schema in `nexo-plugin.toml`:

```toml
[plugin.config]
schema_path = "config.schema.json"   # relative to plugin root
hot_reload = true                    # parsed; wiring lands in 81.4.b
```

The schema validator currently supports the JSONSchema subset
`type` / `required` / `properties` / `additionalProperties` /
`enum`. Plugins needing `oneOf` / `$ref` / `pattern` will get
richer validation in a future 81.4.c slice — for now, those
keywords pass through silently.

Inside your plugin, consume `ctx.plugin_config` (an
`Arc<serde_yaml::Value>`):

```rust
let api_token = ctx
    .plugin_config
    .get("api_token")
    .and_then(serde_yaml::Value::as_str)
    .ok_or_else(|| anyhow::anyhow!("api_token missing"))?;
```

When the operator hasn't placed any config files, the value is
an empty mapping — your plugin sees `Value::Mapping(empty)`,
not `Null`. Plugins with all-optional fields boot cleanly
without operator action.

## Future capability extensions

Phase 81.28 — subprocess plugins that contribute new
**channel kinds**, **LLM providers**, **memory backends**, or
**HookInterceptor IDs** declare them via an additive
`[plugin.extends]` manifest section:

```toml
[plugin.extends]
channels         = ["slack"]              # paired with Phase 81.24 wrapper
llm_providers    = ["cohere"]             # paired with Phase 81.25
memory_backends  = ["pinecone"]           # paired with Phase 81.26
hooks            = ["pii_redact"]         # paired with Phase 81.27
```

Each list names the IDs the plugin contributes. Validation
rules + the canonical schema live in
[Plugin contract §2.1](./contract.md#21-extends-section-phase-8128).
Daemon dispatch wiring (actually populating the matching
registry slots) ships per-registry across Phase 81.24-27 — the
schema is shipped today so subprocess plugin authors can
declare intent ahead of those wrappers landing.

## Local dev loop conventions

- **`nexo plugin run <path>`** — boots the daemon with one
  local plugin overriding discovery; the rest of the system
  (broker, agents, channels) runs as configured.
- **`nexo plugin run <path> --no-daemon-config`** — same, but
  clears `cfg.agents.agents` so the plugin runs in isolation
  for contract debugging.
- **Rebuild → respawn** — Phase 81.10 hot-reload re-walks
  `search_paths` periodically, so a fresh `cargo build` triggers
  the daemon to respawn the subprocess automatically. No
  `--watch` flag yet (Phase 31.7.b deferred).

## Next steps

- [Rust SDK](./rust-sdk.md) — full Rust API + manifest example.
- [Python SDK](./python-sdk.md), [TypeScript SDK](./typescript-sdk.md), [PHP SDK](./php-sdk.md) — language-specific
  references with the same shape.
- [Plugin contract](./contract.md) — wire spec; read this once
  and you can debug any SDK.
- [Patterns (8 common shapes)](./patterns.md) — pre-baked
  designs for channel plugins, pollers, hybrid bridges.
- [Publishing a plugin](./publishing.md) — asset naming
  convention + 4-job CI workflow shape.
- [Signing & publishing](./signing-and-publishing.md) — cosign
  keyless tutorial that operators on `--require-signature`
  need.
- [Plugin trust (`trusted_keys.toml`)](../ops/plugin-trust.md)
  — operator-side verification policy your readers will
  configure to trust your releases.
