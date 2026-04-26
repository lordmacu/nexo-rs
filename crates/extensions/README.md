# nexo-extensions

> The extension system for Nexo — `plugin.toml` manifest schema, capability declarations, discovery, lifecycle hooks, and **two runtimes** (stdio for local processes + NATS for distributed extensions).

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`plugin.toml` manifest** — declarative description an
  extension drops next to its binary / script. Operator-side
  fields (name, version, description) + technical fields
  (`capabilities.{tools,pollers,mcp,setup}`, `runtime`,
  `args`).
- **Discovery** — recursive walk of `extensions/` (or
  configured directories) parsing every `plugin.toml` it
  finds. Diagnostics surfaced via `DiagnosticLevel::{Info,
  Warn, Error}` so the operator sees broken manifests at boot.
- **Capability registration** — extensions declare what they
  add (tools, pollers, MCP servers, setup wizard entries);
  the runtime collects the union and registers each on the
  appropriate registry.
- **Lifecycle hooks** — `on_install`, `on_enable`, `on_disable`,
  `on_uninstall` so an extension can run setup steps without
  needing manual operator intervention.
- **stdio runtime** — spawn the extension binary as a
  subprocess, communicate via JSON-RPC 2.0 over line-delimited
  stdio. The MCP client (in `nexo-mcp`) reuses this transport.
- **NATS runtime** — for extensions running as separate
  daemons on remote hosts; communication via dedicated NATS
  topics with the same JSON-RPC envelope.
- **Hot-reload** — Phase 18 watches `extensions/` for new /
  removed manifests + reloads the registries atomically.

## Public API

```rust
pub struct ExtensionDiscovery { /* … */ }

impl ExtensionDiscovery {
    pub fn new(config: &ExtensionsConfig, dirs: &[PathBuf]) -> Self;
    pub async fn discover(&self) -> DiscoveryReport;
}

pub fn collect_mcp_declarations(report: &DiscoveryReport, disabled: &[String]) -> Result<Vec<McpDeclaration>>;

pub mod runtime {
    pub mod stdio { /* ChildExtension, spawn, recv */ }
    pub mod wire { /* JSON-RPC encode / decode */ }
}
```

## Architecture

```
extensions/
├── my-skill/
│   ├── plugin.toml      ← manifest
│   ├── handler.py       ← stdio runtime
│   └── README.md
└── another/
    └── plugin.toml      ← runtime: nats
```

Boot:

1. `ExtensionDiscovery::discover` walks `extensions/`, parses
   each `plugin.toml`.
2. Capabilities are collected per kind (tools, pollers, MCP).
3. Tool registry, poller runner, MCP runtime, and setup
   wizard each consume their slice.
4. stdio runtimes don't spawn until the agent first calls a
   tool from that extension; NATS runtimes are expected to
   be running independently.

## Install

```toml
[dependencies]
nexo-extensions = "0.1"
```

## Documentation for this crate

- [Extension manifest](https://lordmacu.github.io/nexo-rs/extensions/manifest.html)
- [stdio runtime](https://lordmacu.github.io/nexo-rs/extensions/stdio.html)
- [NATS runtime](https://lordmacu.github.io/nexo-rs/extensions/nats.html)
- [Extension CLI](https://lordmacu.github.io/nexo-rs/extensions/cli.html)
- [Extension templates](https://lordmacu.github.io/nexo-rs/extensions/templates.html)
- [Python extension recipe](https://lordmacu.github.io/nexo-rs/recipes/python-extension.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
