# nexo-plugin-browser

> Chrome DevTools Protocol (CDP) browser channel plugin for Nexo agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Launches Chrome / Chromium (including **Termux Chromium** on Android) and drives it via CDP — no Selenium / WebDriver.
- **Element refs**: stable handles across navigation so the LLM can re-target elements without re-finding them.
- Command queue + event loop so the agent can interleave actions and observations.
- **Session persistence**: cookies, localStorage, profile dir survive restarts.

## Install

```toml
[dependencies]
nexo-plugin-browser = "0.1"
```

## Documentation for this crate

- [Browser plugin](https://lordmacu.github.io/nexo-rs/plugins/browser.html)
- [Termux install](https://lordmacu.github.io/nexo-rs/getting-started/install-termux.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
