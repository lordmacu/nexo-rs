# nexo-core

> The runtime engine of [Nexo](https://github.com/lordmacu/nexo-rs) — `AgentRuntime`, `SessionManager`, `EventBus`, `Heartbeat`, plus the LLM-facing tool registry and per-binding capability resolver. Every other crate in the workspace depends on this one.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** this crate
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

### Runtime

- **`AgentRuntime`** — owns every agent's lifecycle, intake hot
  path (per-binding rate limiter, pairing gate, credential
  resolver, redaction, link-understanding pre-fetch), and the
  agent loop that drives turns + dispatches tool calls.
- **`SessionManager`** — per-`(agent_id, channel,
  instance, sender_id)` session, debounced inbound + queue cap.
- **`AgentBehavior` trait** — what an agent actually does on
  each turn. The default `LlmBehavior` runs the LLM agent loop;
  custom behaviours plug in here for non-LLM agents.

### Tools

- **`ToolRegistry`** + per-session filtered cache (Phase 18
  hot-reload aware).
- **Built-in tools** — `web_search`, `web_fetch` (Phase 21 +
  W-2), `taskflow`, `memory_*`, `session_logs`, `delegation`,
  `heartbeat`, `mcp.*`, `forge_memory_checkpoint` /
  `memory_history` (workspace-git memory).
- **`ToolHandler` trait** for declaring new tools.

### Memory + transcripts

- **Transcript writer** + FTS5 index for `session_logs search`.
- **Redaction module** — opt-in regex redactor with 6 built-in
  patterns (Bearer JWT, sk-/sk-ant- API keys, AWS access key,
  hex token, home path) + operator-defined extras.
- **Workspace-git memory** (Phase 10.9) — per-agent git repo
  for SOUL.md / MEMORY.md persistence, dreaming sweeps,
  forensics.

### Capability resolution

- **`EffectiveBindingPolicy`** (Phase 16) — merges agent-level
  + binding-level policy at intake; cached on `AgentContext`
  for the turn.
- **Per-agent credentials** via `nexo-auth` resolver (Phase
  17), wired into every tool that needs upstream API keys.
- **Hot-reload** (Phase 18) — `ArcSwap<RuntimeSnapshot>` for
  atomic config swap mid-session.

### Phase 21 — link understanding

- **`LinkExtractor`** — fetches user-shared URLs, runs the
  readability-shaped extractor (Phase 21 L-2), caches in an
  LRU. Surfaces summaries via the `# LINK CONTEXT` block in
  the prompt.

### Pairing (Phase 26)

- **`PairingGate`** consulted on every inbound before the
  per-sender rate limiter; `PairingAdapterRegistry` resolves
  the per-channel adapter for outbound challenge delivery.

## Public API surface

This crate is large; the most operator-relevant types:

| Type | Purpose |
|---|---|
| `AgentRuntime` | Top-level runtime constructor |
| `AgentBehavior` trait | Plug-in custom non-LLM behaviours |
| `ToolHandler` trait | Register new tools |
| `EffectiveBindingPolicy` | Per-binding merged config |
| `LinkExtractor` | Fetch + extract URL contents |
| `WebSearchTool`, `WebFetchTool`, `TaskFlowTool`, etc. | Built-in tools |
| `redaction::Redactor` | Pre-persistence redaction |

## Install

```toml
[dependencies]
nexo-core = "0.1"
```

## Documentation for this crate

- [Architecture overview](https://lordmacu.github.io/nexo-rs/architecture/overview.html)
- [Agent runtime](https://lordmacu.github.io/nexo-rs/architecture/agent-runtime.html)
- [Transcripts (FTS + redaction)](https://lordmacu.github.io/nexo-rs/architecture/transcripts.html)
- [Link understanding](https://lordmacu.github.io/nexo-rs/ops/link-understanding.html)
- [Web fetch](https://lordmacu.github.io/nexo-rs/ops/web-fetch.html)
- [Per-agent credentials](https://lordmacu.github.io/nexo-rs/config/credentials.html)
- [Hot-reload](https://lordmacu.github.io/nexo-rs/ops/hot-reload.html)
- [Agent-to-agent delegation recipe](https://lordmacu.github.io/nexo-rs/recipes/agent-to-agent.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
