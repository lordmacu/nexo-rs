# nexo-setup

> Operator setup machinery for Nexo — interactive `nexo setup` wizard, declarative service catalog, capability inventory + audit (`nexo doctor capabilities`), and credential check.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`ServiceDef` catalog** — declarative description of every
  configurable surface (LLM providers, channel plugins, skills,
  pollers, infra). Each `ServiceDef` owns a list of
  `FieldDef`s (label, kind, required-flag, target file/env,
  validator).
- **Interactive wizard** — `nexo setup` walks the catalog,
  prompts the operator for each field, and writes secrets into
  `secrets/` files (mode 0600) + non-secret values into
  `config/*.yaml`.
- **Imperative service ops** — `services_imperative.rs` for
  programmatic enable/disable from the admin-ui (Phase 29) so
  the wizard logic + the web UI share one decision tree.
- **Capability inventory** — `nexo doctor capabilities [--json]`
  enumerates every `*_ALLOW_*`, `*_REVEAL`, `*_PURGE` env
  toggle armed in the operator's shell, with state + risk
  + revoke-hint per entry.
- **Credential check** — `nexo doctor credentials` validates
  that every configured agent has the credentials it claims
  (Telegram bot token resolves, Google OAuth scopes match
  what's needed, …) before the daemon takes traffic.
- **YAML patcher** — surgically rewrites a single key in a
  config file without losing comments / formatting (used by
  the wizard so the `config/` tree stays operator-readable
  after edits).
- **Telegram link helper** — generates the `t.me/<bot>?start=…`
  deep link that ties an operator's chat to the agent on
  first pairing.

## Where it's used

- The `nexo setup` CLI subcommand
- The admin-ui A1 onboarding flow
- `nexo doctor capabilities` + `nexo doctor credentials`
- Per-extension setup hooks declared via `capabilities.setup`

## Install

```toml
[dependencies]
nexo-setup = "0.1"
```

## Documentation for this crate

- [Setup wizard](https://lordmacu.github.io/nexo-rs/getting-started/setup-wizard.html)
- [Capability toggles](https://lordmacu.github.io/nexo-rs/ops/capabilities.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
