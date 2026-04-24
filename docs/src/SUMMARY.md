# Summary

[Introduction](./introduction.md)

# Getting started

- [Installation](./getting-started/installation.md)
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

---

[Contributing](./contributing.md)
[License](./license.md)
