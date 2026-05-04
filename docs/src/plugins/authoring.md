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
