# Layering — Plugin / Extension / MCP / Skill

Four orthogonal ways the agent gains capabilities. Pick the right one
when adding a new integration.

## Plugin — in-process Rust

Compiled into the `agent` binary. Implements the `Plugin` trait
(`crates/core/src/agent/plugin.rs`). Gets broker access, shared
memory, a spawned event loop.

**Homes:** `crates/plugins/<name>/`
**Today:** `browser`, `whatsapp`, `telegram`, `email`, `google`

**Use when:**
- The integration has a persistent event loop (WebSocket, long-poll).
- It publishes/subscribes on the broker.
- Tight coupling to runtime state is fine.

**Register:** list the plugin id under `agents.<id>.plugins:` in
`agents.yaml`.

## Extension — external process

Separate binary/script discovered via `plugin.toml`. Spawned by the
extension runtime. Talks JSON-RPC over stdio or NATS. Language-agnostic.

**Homes:** `extensions/<name>/`
**Today:** `brave-search`, `cloudflare`, `github`, `yt-dlp`, `ssh-exec`,
`wikipedia`, etc.

**Use when:**
- Wrapping a CLI (yt-dlp, kubectl).
- Calling a third-party API that's stateless per invocation.
- You'd rather write Python/Bash than Rust.

**Register:** drop directory under `extensions/`, enable in
`config/extensions.yaml`. Tools appear as `ext_<name>_<tool>` for every
agent.

## MCP — Model Context Protocol (standard)

Anthropic's cross-vendor protocol. The agent is both **client** (talks
to external MCP servers) and **server** (exposes its own tools as MCP
so Claude Desktop / Claude Code can consume them).

**Code:** `crates/mcp/`

**Use when:**
- You want to share tools with other LLM apps (Claude Desktop, etc.).
- Consuming someone else's MCP server (official filesystem, linear,
  slack MCPs).

**Not the same as an extension:** extensions use our internal JSON-RPC;
MCPs follow the public spec.

## Skill — documentation only

A `SKILL.md` file under `skills/<name>/`. No code. Tells the LLM when
and how to orchestrate tools that already exist (from plugins,
extensions, MCPs).

**Structure of `SKILL.md`:**

```
---
name: …
description: …
requires:
  bins: []
  env: []
---

## Use when
…

## Do not use when
…

## Canonical workflow
…

## Tool reference
…
```

**Register:** list the skill id under `agents.<id>.skills:` in
`agents.yaml`. On boot each `SKILL.md` is read and prepended to the
agent's system prompt.

## Decision table

| Need | Pick |
|------|------|
| New messaging channel (Slack, Discord) | **Plugin** |
| Wrapper for an existing CLI (ffmpeg, kubectl) | **Extension** |
| Integration with a third-party API (Jira, AWS) | **Extension** or **MCP** |
| Teach the LLM a workflow that uses existing tools | **Skill** |
| Expose the agent to Claude Desktop | **MCP server** |

## "Native tool bundle" (antipattern, avoid)

A fifth pattern existed briefly: ToolHandler instances compiled into
`nexo-core` with shared `Arc<State>` (Google OAuth, Anthropic OAuth).
This blurred the layers — the runtime shipped integration-specific
machinery even for agents that didn't use it.

As of the 2026-04-24 refactor these live as proper plugins
(`crates/plugins/google`). Any future integration that needs shared
state but no event loop should still go into `crates/plugins/<name>/`,
implementing the `Plugin` trait or just exposing a `register_tools`
helper.
