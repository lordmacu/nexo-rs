# nexo-config

> YAML configuration loader for Nexo with `${ENV_VAR}` + `${file:/path}` resolution, drop-in agent merging, and `serde(deny_unknown_fields)` schema validation that catches typos at boot.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`AppConfig::load(dir)`** — single entry point that reads
  the full config tree (`agents.yaml`, `broker.yaml`,
  `llm.yaml`, `memory.yaml`, optional
  `extensions.yaml`/`mcp.yaml`/`mcp_server.yaml`/
  `pollers.yaml`/`runtime.yaml`/`taskflow.yaml`/
  `transcripts.yaml`/`pairing.yaml`).
- **Strict schema** — every config struct uses
  `#[serde(deny_unknown_fields)]` so a typo (`agnts:` instead
  of `agents:`) fails loud at boot.
- **`${ENV_VAR}`** placeholders — resolves at load time;
  panics with a clear error when an env-mandated var is
  missing.
- **`${file:/path}`** placeholders — reads a value from disk
  (typically `/run/secrets/...` for Docker secret bundles)
  without keeping it in YAML.
- **Drop-in agents** — `config/agents.d/*.yaml` files merge
  into the top-level `agents:` list so business-sensitive
  agents (full prompts, pricing tables) can stay in a
  gitignored side directory.
- **Path resolution** — relative `skills_dir`, `workspace`,
  `transcripts_dir`, `extra_docs` paths are joined against
  the config dir at load, so the config is portable across
  cwds.
- **Hot-reload aware** — Phase 18 `RuntimeSnapshot` consumes
  this loader; the file watcher triggers a reparse + atomic
  swap when YAMLs change.
- **Per-agent + per-binding** — both surfaces parse here;
  the runtime then computes `EffectiveBindingPolicy`
  (Phase 16) per inbound to merge agent-level + binding-level
  settings.

## Public API

```rust
pub struct AppConfig {
    pub agents: AgentsConfig,
    pub broker: BrokerConfig,
    pub llm: LlmConfig,
    pub memory: MemoryConfig,
    pub plugins: PluginsConfig,
    pub extensions: Option<ExtensionsConfig>,
    pub mcp: Option<McpConfig>,
    pub mcp_server: Option<McpServerConfig>,
    pub runtime: RuntimeConfig,
    pub pollers: Option<PollersConfig>,
    pub taskflow: TaskflowConfig,
    pub transcripts: TranscriptsConfig,
    pub pairing: Option<PairingInner>,
}

impl AppConfig {
    pub fn load(dir: &Path) -> Result<Self>;
    pub fn load_for_mcp_server(dir: &Path) -> Result<McpServerBootConfig>;
}
```

## Install

```toml
[dependencies]
nexo-config = "0.1"
```

## Documentation for this crate

- [Configuration layout](https://lordmacu.github.io/nexo-rs/config/layout.html)
- [Drop-in agents](https://lordmacu.github.io/nexo-rs/config/drop-in.html)
- [Agents config](https://lordmacu.github.io/nexo-rs/config/agents.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
