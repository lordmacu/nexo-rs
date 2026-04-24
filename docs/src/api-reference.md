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
| `agent-core` | `Agent` trait, `AgentRuntime`, `SessionManager`, `ToolRegistry`, `HookRegistry`, agent-facing tools (`memory`, `taskflow`, `self_report`, `delegate`, `workspace_git`). |
| `agent-broker` | `Broker` trait (`NatsBroker`, `LocalBroker`), disk queue, DLQ. |
| `agent-llm` | `LlmClient` trait, MiniMax / Anthropic / OpenAI-compat / Gemini clients, retry + rate limiter. |
| `agent-memory` | Short-term / long-term / vector types, `LongTermMemory` API. |
| `agent-config` | YAML struct types, env/file placeholder resolution. |
| `agent-extensions` | `ExtensionManifest`, `ExtensionDiscovery`, `StdioRuntime`, CLI. |
| `agent-mcp` | MCP client + server primitives. |
| `agent-taskflow` | `Flow`, `FlowStore`, `FlowManager`, `WaitEngine`. |
| `agent-resilience` | `CircuitBreaker`. |
| `agent-setup` | Wizard field registry, YAML patcher. |
| `agent-tunnel` | Cloudflared tunnel helper. |
| `agent-auth` | Per-agent credential gauntlet, resolver, audit. |
| `agent-plugin-*` | Channel plugins (browser, whatsapp, telegram, email, google, gmail-poller). |

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

# Open the agent-core rustdoc in a browser:
cargo doc -p agent-core --no-deps --open
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
