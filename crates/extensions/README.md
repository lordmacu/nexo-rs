# nexo-extensions

> Extension manifest, discovery, lifecycle, and stdio/NATS runtimes for Nexo.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`plugin.toml` manifest** schema: extension metadata, declared tools, capabilities, transport, lifecycle hooks.
- **Discovery** scans `extensions/` directories and validates manifests at boot.
- **Stdio runtime**: spawn the extension as a child process, JSON-RPC over stdin/stdout — works for any language.
- **NATS runtime**: extension subscribes to NATS topics, can run remotely / out-of-process.
- **Lifecycle hooks**: `on_install`, `on_start`, `on_stop`, `on_event`.
- **CLI scaffolding**: `nexo ext new <name>` to create a new extension from a template.

## Install

```toml
[dependencies]
nexo-extensions = "0.1"
```

## Documentation for this crate

- [Manifest (plugin.toml)](https://lordmacu.github.io/nexo-rs/extensions/manifest.html)
- [Stdio runtime](https://lordmacu.github.io/nexo-rs/extensions/stdio.html)
- [NATS runtime](https://lordmacu.github.io/nexo-rs/extensions/nats.html)
- [CLI](https://lordmacu.github.io/nexo-rs/extensions/cli.html)
- [Templates](https://lordmacu.github.io/nexo-rs/extensions/templates.html)
- [Recipe — Python extension](https://lordmacu.github.io/nexo-rs/recipes/python-extension.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
