# nexo-rs vs OpenClaw

[OpenClaw](https://github.com/openclaw/openclaw) is the closest
reference point in the multi-channel-agent-gateway space. nexo-rs
mined OpenClaw's plugin SDK, channel boundaries, and skills layout
for ideas, then rebuilt the runtime in Rust with stricter
operational guarantees. This page lays out the differences honestly
— including where OpenClaw still has the edge.

## Substrate

| Dimension | OpenClaw | nexo-rs |
|-----------|----------|---------|
| Language | TypeScript | Rust |
| Runtime | Node 22+ | none — single statically-linked binary |
| Install footprint | `pnpm install` over ~42 runtime deps + 24 dev deps | one binary, **34 MB** built (29 MB stripped, 13 MB gzipped) |
| Cold-start | `node` boot + module resolution | direct `exec` — sub-100ms to `agent serve` |
| Mobile target | feasible with Termux + Node | first-class on Termux, no root, no Docker |
| Memory safety | runtime errors | Rust ownership: data races, use-after-free, null deref refused at compile |

The single-binary shape is the reason nexo-rs runs comfortably on a
phone (Termux) and on a fresh VPS without a Node ecosystem
underneath. `cargo build --release` and ship `target/release/agent`
— that is the whole deliverable.

## Process & messaging

| Dimension | OpenClaw | nexo-rs |
|-----------|----------|---------|
| Process model | single Node process | multi-process via NATS, in-process `LocalBroker` fallback when NATS is offline |
| Subject namespace | n/a (in-process buses) | `plugin.inbound.<plugin>[.instance]` / `plugin.outbound.…` / `agent.route.<id>` / `taskflow.resume` |
| Fault tolerance | best-effort | `NatsBroker` wraps every publish in a `CircuitBreaker`; failures spill to a SQLite-backed disk queue and drain on reconnect |
| At-least-once delivery | n/a | drain path documented as at-least-once; consumers dedupe by `event.id` |
| DLQ | n/a | failed events land in `dead_letters` after 3 attempts; `agent dlq list/replay/purge` from the CLI |
| Subscription survival | restart | NATS subscriptions auto-resubscribe on reconnect with backoff (250 ms → 10 s) |

## Hot reload

| Dimension | OpenClaw | nexo-rs |
|-----------|----------|---------|
| Config change | restart | `agent reload` (or file-watcher trigger) swaps a `RuntimeSnapshot` via `ArcSwap` — in-flight turns finish on the old snapshot, the next event picks up the new one |
| Watched files | — | `agents.yaml`, `agents.d/*.yaml`, `llm.yaml` (extra paths via `runtime.yaml`) |
| Per-agent reload channel | — | mpsc to each `AgentRuntime`, the coordinator drains acks to confirm |

## Per-agent capability sandbox

OpenClaw's plugin allowlist is global to the gateway. nexo-rs
pushes the allowlist down to the agent and the binding (the
inbound channel surface):

```yaml
agents:
  - id: kate
    plugins: [whatsapp, telegram, browser, taskflow]
    allowed_tools: ["whatsapp_*", "browser_navigate", "memory_*"]
    outbound_allowlist:
      whatsapp: ["+57…"]
      telegram: [123456789]
    skill_overrides:
      ffmpeg-tools: warn
    accept_delegates_from: ["ana"]
    inbound_bindings:
      - plugin: whatsapp
        instance: kate_wa
        # per-binding overrides for the same agent
        allowed_tools: ["whatsapp_*"]
        outbound_allowlist:
          whatsapp: ["+57…"]
```

What that buys:

- An LLM running under `kate` cannot send messages to a number
  not in `outbound_allowlist`, even if a prompt injection asks it
  to.
- Two channels exposed to the same agent (sales WA, private TG)
  carry **different** capability surfaces — the sales binding
  doesn't get the private one's tool set.
- Skill modes (`strict` / `warn` / `disable`) are decided per
  agent, with explicit `requires.bin_versions` semver constraints
  (probed at boot, process-cached).

## Secrets

| Dimension | OpenClaw | nexo-rs |
|-----------|----------|---------|
| Credential resolution | env vars | `agents.<id>.credentials` block per channel; resolver maps to per-channel stores (gauntlet validates at boot) |
| 1Password | n/a | `op` CLI extension + `inject_template` tool: render `{{ op://Vault/Item/field }}` and pipe to allowlisted commands without exposing the secret |
| Audit log | n/a | append-only JSONL at `OP_AUDIT_LOG_PATH`: every `read_secret` and `inject_template` records `agent_id`, `session_id`, fingerprint, `reveal_allowed` — never the value |
| Capability inventory | n/a | `agent doctor capabilities [--json]` enumerates every write/reveal env toggle (`OP_ALLOW_REVEAL`, `CLOUDFLARE_*`, `DOCKER_API_*`, `PROXMOX_*`, `SSH_EXEC_*`) with state + risk |

## Transcripts

OpenClaw stores transcripts as JSONL and `grep`s them. nexo-rs
keeps the JSONL (source of truth) and adds:

- **SQLite FTS5 index** (`data/transcripts.db`) — write-through
  from `TranscriptWriter::append_entry`. The `session_logs search`
  agent tool uses `MATCH` queries with phrase-escaped user input
  so operator strings can't inject FTS operators.
- **Pre-persistence redactor** (opt-in) — regex pass over content
  before write. 6 built-in patterns (Bearer JWT, `sk-…`,
  `sk-ant-…`, AWS access keys, 64+ hex tokens, home paths) plus
  operator-defined `extra_patterns`. JSONL and FTS receive the
  same redacted text.
- **Atomic header writes** — `OpenOptions::create_new(true)` so 16
  concurrent first-appends to the same session result in exactly
  one header line.

## Durable workflows

OpenClaw doesn't ship a durable-flow primitive. nexo-rs has
**TaskFlow**:

- `taskflow` LLM tool with actions `start | status | advance | wait
  | finish | fail | cancel | list_mine`.
- Three wait conditions: `Timer { at }`, `ExternalEvent { topic,
  correlation_id }`, `Manual`.
- Single global `WaitEngine` ticks every 5 s (configurable),
  resumes flows whose deadlines have passed.
- `taskflow.resume` NATS subject lets external services wake
  `external_event` flows: publish `{flow_id, topic,
  correlation_id, payload}` and the bridge calls
  `try_resume_external`.
- `agent flow list/show/cancel/resume` from the CLI.
- Guardrails: `timer_max_horizon` (default 30 days) blocks
  unbounded waits; non-empty topic + correlation_id required for
  `external_event`.

## LLM auth

| Dimension | OpenClaw | nexo-rs |
|-----------|----------|---------|
| Anthropic | API key | API key **and** `claude_subscription` OAuth PKCE flow — uses the operator's Claude Code subscription quota instead of API billing |
| MiniMax | API key | API key **and** Token Plan / Coding Plan OAuth bundle (`api_flavor: anthropic_messages`) |
| OpenAI-compat | API key | API key + DeepSeek wired out of the box (OpenAI-compat reuse) |
| Gemini | not in core | first-class client |

## MCP

OpenClaw supports MCP as a client. nexo-rs is **both**:

- **Client** — stdio and HTTP transports, full tool / resource /
  prompt catalog, `tools/list_changed` hot-reload.
- **Server** — `agent mcp-server` exposes the agent's own tools
  (filtered by allowlist) over stdio for Claude Desktop / Cursor /
  any MCP-aware host. Proxy tools (`ext_*`, `mcp_*`) are
  unconditionally hidden so the agent doesn't become an open
  relay.

## Build size

```text
target/release/agent             34 MB
target/release/agent (stripped)  29 MB
target/release/agent (.gz -9)    13 MB
```

For comparison, an OpenClaw install (Node + `node_modules` after
`pnpm install`) sits in the hundreds of megabytes — most of it
needed at runtime, not just build-time.

## Where OpenClaw is still ahead

Honest list:

- **Installer & onboarding flow** — OpenClaw's `openclaw doctor`
  family and the bundled installer give a smoother first-run UX
  than nexo-rs's `agent setup` wizard, especially for non-Rust
  developers.
- **TS familiarity** — the JS / TS audience for plugin authors is
  larger than the Rust audience; if your team writes mostly
  TypeScript, contributing back to OpenClaw is faster.
- **Track record** — OpenClaw has a longer release history,
  more maintainers, and more shipped extensions in the wild.
- **Apps surface** — OpenClaw ships iOS / Android / macOS
  companion apps; nexo-rs only ships the daemon and the
  loopback web admin (admin-ui Phase A0–A11 still in progress).

## Summary

If you want operational guarantees (single binary, fault-tolerant
broker, per-agent sandbox, durable workflows, secrets audit) and
you're OK with Rust, **nexo-rs**.

If you want fast onboarding, a TS plugin ecosystem, and the
OpenClaw apps, **OpenClaw**.

The two projects share enough vocabulary that moving an extension
between them is mostly a port, not a rewrite. The plugin SDK
shape (`stdio`-spoken JSON-RPC + a `plugin.toml` manifest) is
deliberately compatible.
