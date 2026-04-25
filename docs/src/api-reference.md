# API reference (rustdoc)

Every public type, trait, function, and module in the nexo-rs
workspace is documented via `cargo doc`. The CI workflow runs
`cargo doc --workspace --no-deps` and publishes the output under
`/api/` on the same GitHub Pages deployment as this book.

## Open the rustdoc

- **Published site:** <https://lordmacu.github.io/nexo-rs/api/>
- **Local build:** `cargo doc --workspace --no-deps --open`

## What's there

One rustdoc page per workspace crate:

| Crate | Contents |
|-------|----------|
| `agent` | Top-level binary — mostly wiring; see `src/main.rs`. |
| `nexo-core` | `Agent` trait, `AgentRuntime`, `SessionManager`, `ToolRegistry`, `HookRegistry`, agent-facing tools (`memory`, `taskflow`, `self_report`, `delegate`, `workspace_git`). |
| `nexo-broker` | `Broker` trait (`NatsBroker`, `LocalBroker`), disk queue, DLQ. |
| `nexo-llm` | `LlmClient` trait, MiniMax / Anthropic / OpenAI-compat / Gemini clients, retry + rate limiter. |
| `nexo-memory` | Short-term / long-term / vector types, `LongTermMemory` API. |
| `nexo-config` | YAML struct types, env/file placeholder resolution. |
| `nexo-extensions` | `ExtensionManifest`, `ExtensionDiscovery`, `StdioRuntime`, CLI. |
| `nexo-mcp` | MCP client + server primitives. |
| `nexo-taskflow` | `Flow`, `FlowStore`, `FlowManager`, `WaitEngine`. |
| `nexo-resilience` | `CircuitBreaker`. |
| `nexo-setup` | Wizard field registry, YAML patcher. |
| `nexo-tunnel` | Cloudflared tunnel helper. |
| `nexo-auth` | Per-agent credential gauntlet, resolver, audit. |
| `nexo-plugin-*` | Channel plugins (browser, whatsapp, telegram, email, google, gmail-poller). |

## When to read rustdoc vs the book

| Goal | Start here |
|------|------------|
| Understand a subsystem's purpose | this book |
| Read a specific trait's methods / signatures | rustdoc |
| Wire two subsystems together | book → rustdoc |
| Embed a crate in your own binary | rustdoc |
| Audit what's public API | rustdoc (anything not in rustdoc is internal) |

## Building locally

```bash
# All crates, no dependencies:
cargo doc --workspace --no-deps

# Open the nexo-core rustdoc in a browser:
cargo doc -p nexo-core --no-deps --open
```

Warnings are rejected in CI (`RUSTDOCFLAGS=-D warnings`). Run the
same locally before pushing if you edited doc comments:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

## Public-API stability

The workspace has **not** committed to semver-level stability yet.
Public signatures change between code phases; follow `PHASES.md` and
commit history when upgrading.

## Cross-links

- [Contributing](./contributing.md) — how to add `///` docs when you
  touch public surface
- [Architecture overview](./architecture/overview.md) — the mental
  model that rustdoc fills in the fine detail for
