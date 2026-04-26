# nexo-plugin-browser

> Browser automation plugin for Nexo — Chrome DevTools Protocol (CDP) client + element-ref system for agents that need to read or interact with live web pages.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **CDP client** over the local Chrome / Chromium WebSocket
  endpoint. The plugin launches a managed Chrome subprocess
  (configurable bin path for Termux / Docker) and connects to
  its `--remote-debugging-port`.
- **Per-session BrowserContext** — each agent session can hold
  its own browsing context (cookies, localStorage) so two
  agents on the same daemon don't share credentials by accident.
- **Element ref system** — elements are referenced by stable
  IDs the runtime hands back to the agent (instead of brittle
  CSS selectors that the LLM has to memorise across turns).
- **Built-in commands** — `navigate`, `screenshot`, `query`
  (CSS / XPath), `click`, `type`, `evaluate` (run JS), `wait`,
  `extract_text`, `network_idle`. All reachable via the
  agent's tool registry.
- **Event loop** — surfaces page-loaded / DOM-mutation /
  console-message events to the agent so it can react to
  long-running interactions instead of polling.
- **Termux support** — when `args` includes the
  `--no-sandbox --disable-dev-shm-usage` flags, the plugin
  uses chromium-on-Termux happy-path defaults. Phase 4
  hardening kept this working across the rename.

## Configuration

```yaml
plugins:
  browser:
    chrome_bin: /usr/bin/google-chrome   # or chromium for arm64
    headless: true                       # false for debug
    args:
      - --no-sandbox
      - --disable-dev-shm-usage
    user_data_dir: /var/lib/nexo/browser
```

## Wire format

Outbound (agent → browser):

```json
{ "kind": "browser.navigate", "session_id": "...", "url": "https://..." }
{ "kind": "browser.screenshot", "session_id": "...", "selector": "#main" }
```

## Install

```toml
[dependencies]
nexo-plugin-browser = "0.1"
```

## Documentation for this crate

- [Browser plugin guide](https://lordmacu.github.io/nexo-rs/plugins/browser.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
