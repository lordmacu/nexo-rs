# Summary

[Introduction](./introduction.md)

# Getting started

- [Installation](./getting-started/installation.md)
- [Native install (no Docker)](./getting-started/install-native.md)
- [Quick start](./getting-started/quickstart.md)
- [Setup wizard](./getting-started/setup-wizard.md)

# Architecture

- [Overview](./architecture/overview.md)
- [Agent runtime](./architecture/agent-runtime.md)
- [Event bus (NATS)](./architecture/event-bus.md)
- [Fault tolerance](./architecture/fault-tolerance.md)

# Configuration

- [Layout](./config/layout.md)
- [agents.yaml](./config/agents.md)
- [llm.yaml](./config/llm.md)
- [broker.yaml](./config/broker.md)
- [memory.yaml](./config/memory.md)
- [Drop-in agents](./config/drop-in.md)
- [Per-agent credentials](./config/credentials.md)

# LLM providers

- [MiniMax M2.5](./llm/minimax.md)
- [Anthropic / Claude](./llm/anthropic.md)
- [OpenAI-compatible](./llm/openai.md)
- [Rate limiting & retry](./llm/retry.md)

# Plugins

- [WhatsApp](./plugins/whatsapp.md)
- [Telegram](./plugins/telegram.md)
- [Email](./plugins/email.md)
- [Browser (CDP)](./plugins/browser.md)
- [Google / Gmail](./plugins/google.md)

# Memory

- [Short-term](./memory/short-term.md)
- [Long-term (SQLite)](./memory/long-term.md)
- [Vector search](./memory/vector.md)

# Extensions

- [Manifest (plugin.toml)](./extensions/manifest.md)
- [Stdio runtime](./extensions/stdio.md)
- [NATS runtime](./extensions/nats.md)
- [CLI](./extensions/cli.md)
- [Templates](./extensions/templates.md)

# MCP

- [Introduction](./mcp/introduction.md)
- [Client (stdio / HTTP)](./mcp/client.md)
- [Agent as MCP server](./mcp/server.md)

# Skills

- [Catalog](./skills/catalog.md)
- [Gating by env / bins](./skills/gating.md)

# TaskFlow

- [Model](./taskflow/model.md)
- [FlowManager](./taskflow/manager.md)

# Soul, identity & learning

- [Identity](./soul/identity.md)
- [MEMORY.md](./soul/memory.md)
- [Dreaming](./soul/dreaming.md)

# CLI

- [Reference](./cli/reference.md)

# Operations

- [Docker](./ops/docker.md)
- [Metrics (Prometheus)](./ops/metrics.md)
- [Logging](./ops/logging.md)
- [DLQ](./ops/dlq.md)

# Recipes

- [Index](./recipes/index.md)
- [WhatsApp sales agent](./recipes/whatsapp-sales-agent.md)
- [Agent-to-agent delegation](./recipes/agent-to-agent.md)
- [Python extension](./recipes/python-extension.md)
- [MCP server from Claude Desktop](./recipes/mcp-from-claude-desktop.md)
- [NATS with TLS + auth](./recipes/nats-tls-auth.md)

# Architecture Decision Records

- [Index](./adr/index.md)
- [0001 — Single-process runtime](./adr/0001-single-process.md)
- [0002 — NATS as the broker](./adr/0002-nats-broker.md)
- [0003 — sqlite-vec for vector search](./adr/0003-sqlite-vec.md)
- [0004 — Per-agent tool sandboxing](./adr/0004-per-agent-sandbox.md)
- [0005 — Drop-in agents.d/ directory](./adr/0005-drop-in-agents.md)
- [0006 — Workspace-git memory forensics](./adr/0006-workspace-git.md)
- [0007 — WhatsApp via whatsapp-rs](./adr/0007-whatsapp-signal-protocol.md)
- [0008 — MCP dual role](./adr/0008-mcp-dual-role.md)
- [0009 — Dual MIT / Apache-2.0 license](./adr/0009-dual-license.md)

---

[Contributing](./contributing.md)
[License](./license.md)
[API reference (rustdoc) ↗](./api-reference.md)
