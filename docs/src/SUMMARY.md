# Summary

[Introduction](./introduction.md)
[Why nexo-rs](./why.md)
[What you can build](./what-you-can-build.md)
[Install for AI assistants](./install-for-ai.md)

# Getting started

- [Installation](./getting-started/installation.md)
- [Quickstart](./getting-started/quickstart.md)
- [Setup wizard](./getting-started/setup-wizard.md)
- [Verifying releases](./getting-started/verify.md)

# The basics

- [Configuration layout](./config/layout.md)
- [Agents (agents.yaml)](./config/agents.md)
- [LLM providers](./llm/minimax.md)
- [Memory · short-term](./memory/short-term.md)
- [Channels overview](./plugins/whatsapp.md)
- [Skills catalog](./skills/catalog.md)

# Plugin SDKs

- [Plugin contract](./plugins/contract.md)
- [Python SDK](./plugins/python-sdk.md)
- [TypeScript SDK](./plugins/typescript-sdk.md)
- [PHP SDK](./plugins/php-sdk.md)
- [Publishing a plugin](./plugins/publishing.md)

# Extensions

- [Manifest reference](./extensions/manifest.md)
- [Templates](./extensions/templates.md)
- [CLI](./extensions/cli.md)
- [Multi-tenant SaaS](./extensions/multi-tenant-saas.md)
- [State management](./extensions/state-management.md)

# Microapps

- [Getting started (1-hour walkthrough)](./microapps/getting-started.md)
- [Admin RPC](./microapps/admin-rpc.md)
- [Contract](./microapps/contract.md)
- [`agent-creator` reference microapp](./microapps/agent-creator.md)
- [Templates by language](./microapps/templates.md)

# CLI

- [Reference](./cli/reference.md)
- [Background agents (`agent run --bg`)](./cli/agent-bg.md)
- [Migrations CLI](./cli/migrations.md)

# Operations

- [Docker](./ops/docker.md)
- [Metrics (Prometheus)](./ops/metrics.md)
- [Logging](./ops/logging.md)
- [DLQ](./ops/dlq.md)
- [Config hot-reload](./ops/hot-reload.md)
- [Plugin trust (cosign)](./ops/plugin-trust.md)
- [Capability toggles](./ops/capabilities.md)
- [Backup + restore](./ops/backup.md)
- [Memory snapshots](./ops/memory-snapshot.md)
- [Health checks](./ops/health.md)
- [Cost & quota](./ops/cost.md)
- [Privacy toolkit (GDPR)](./ops/privacy.md)
- [Telemetry (opt-in)](./ops/telemetry.md)

# Recipes

- [Index](./recipes/index.md)
- [WhatsApp sales agent](./recipes/whatsapp-sales-agent.md)
- [Agent-to-agent delegation](./recipes/agent-to-agent.md)
- [Python extension](./recipes/python-extension.md)
- [Build a poller module](./recipes/build-a-poller.md)
- [Deploy on Hetzner Cloud](./recipes/deploy-hetzner.md)
- [Deploy on Fly.io](./recipes/deploy-fly.md)
- [Deploy on AWS (EC2)](./recipes/deploy-aws.md)
- [NATS with TLS + auth](./recipes/nats-tls-auth.md)
- [Rotating config without downtime](./recipes/hot-reload.md)

---

# Advanced

The sections below are deep-dive material — architecture
internals, design decisions, operator-facing knobs that aren't
part of day-to-day usage. Skim only when you need it.

# Architecture deep dives

- [Overview](./architecture/overview.md)
- [Agent runtime](./architecture/agent-runtime.md)
- [Event bus (NATS)](./architecture/event-bus.md)
- [Fault tolerance](./architecture/fault-tolerance.md)
- [Transcripts (FTS + redaction)](./architecture/transcripts.md)
- [vs OpenClaw](./architecture/vs-openclaw.md)
- [Driver subsystem](./architecture/driver-subsystem.md)
- [Project tracker + dispatch](./architecture/project-tracker.md)
- [Plan mode](./architecture/plan-mode.md)
- [TodoWrite scratch list](./architecture/todo-write.md)
- [ToolSearch deferred schemas](./architecture/tool-search.md)
- [SyntheticOutput typed-output](./architecture/synthetic-output.md)
- [NotebookEdit Jupyter cells](./architecture/notebook-edit.md)
- [RemoteTrigger outbound publisher](./architecture/remote-trigger.md)
- [REPL stateful sandbox](./architecture/repl.md)
- [LSP tool](./architecture/lsp.md)
- [Cron schedule tools](./architecture/cron-schedule.md)
- [MCP router tools](./architecture/mcp-router-tools.md)
- [ConfigTool gated self-config](./architecture/config-tool.md)
- [Team tools](./architecture/team-tools.md)
- [MCP server exposable catalog](./architecture/mcp-server-exposable.md)
- [Fork subagent infra](./architecture/fork-subagent.md)
- [Agent event firehose](./architecture/agent-event-firehose.md)

# Advanced configuration

- [llm.yaml](./config/llm.md)
- [broker.yaml](./config/broker.md)
- [memory.yaml](./config/memory.md)
- [Drop-in agents](./config/drop-in.md)
- [Per-agent credentials](./config/credentials.md)
- [Pollers (pollers.yaml)](./config/pollers.md)

# LLM providers — full guides

- [Anthropic / Claude](./llm/anthropic.md)
- [OpenAI-compatible](./llm/openai.md)
- [DeepSeek](./llm/deepseek.md)
- [Rate limiting & retry](./llm/retry.md)

# Built-in channels — provider guides

- [Telegram](./plugins/telegram.md)
- [Email](./plugins/email.md)
- [Browser (CDP)](./plugins/browser.md)
- [Google / Gmail](./plugins/google.md)

# Memory subsystems

- [Long-term (SQLite)](./memory/long-term.md)
- [Vector search](./memory/vector.md)

# Extensions — internals

- [Stdio runtime](./extensions/stdio.md)
- [NATS runtime](./extensions/nats.md)
- [1Password integration](./extensions/onepassword.md)

# Microapps — extras

- [Building microapps in Rust](./microapps/rust.md)
- [Testing microapps](./microapps/testing.md)
- [Compliance primitives](./microapps/compliance-primitives.md)
- [Publishing the SDK crates](./microapps/publishing.md)

# MCP

- [Introduction](./mcp/introduction.md)
- [Client (stdio / HTTP)](./mcp/client.md)
- [Agent as MCP server](./mcp/server.md)
- [Channels (Slack / Telegram / iMessage inbound)](./mcp/channels.md)
- [HTTP+SSE transport](./extensions/mcp-server.md)
- [Building an MCP server extension](./extensions/mcp-server-extension.md)

# Skills — internals

- [Gating by env / bins](./skills/gating.md)
- [Dependencies (modes + bin versions)](./skills/dependencies.md)

# TaskFlow

- [Model](./taskflow/model.md)
- [FlowManager](./taskflow/manager.md)
- [Wait / resume](./taskflow/wait.md)

# Soul, identity & learning

- [Identity](./soul/identity.md)
- [MEMORY.md](./soul/memory.md)
- [Dreaming](./soul/dreaming.md)

# Assistant mode

- [Autonomous agent — capabilities overview](./agents/autonomous-overview.md)
- [Assistant mode (system addendum)](./agents/assistant-mode.md)
- [Auto-approve dial](./agents/auto-approve.md)
- [AWAY_SUMMARY digest](./agents/away-summary.md)
- [Multi-agent coordination](./agents/multi-agent-coordination.md)
- [Coordinator mode](./agents/coordinator-mode.md)
- [Worker mode](./agents/worker-mode.md)
- [Proactive mode](./agents/proactive-mode.md)

# Operations — extras

- [Compact tiers](./ops/compact-tiers.md)
- [Memdir scanner](./ops/memdir-scanner.md)
- [Bash safety knobs](./ops/bash-safety.md)
- [Channel doctor (MCP)](./ops/channel-doctor.md)
- [Webhook receiver](./ops/webhook-receiver.md)
- [Event subscribers](./ops/events.md)
- [Per-binding tool rate-limits](./ops/per-binding-rate-limits.md)
- [Context optimization](./ops/context-optimization.md)
- [Link understanding](./ops/link-understanding.md)
- [Web search](./ops/web-search.md)
- [Web fetch](./ops/web-fetch.md)
- [Pairing](./ops/pairing.md)
- [Benchmarks](./ops/benchmarks.md)

# Install — alternatives

- [Native install (no Docker)](./getting-started/install-native.md)
- [Debian / Ubuntu (.deb)](./getting-started/install-deb.md)
- [Fedora / RHEL / Rocky (.rpm)](./getting-started/install-rpm.md)
- [Termux (Android)](./getting-started/install-termux.md)
- [Nix flake](./getting-started/install-nix.md)
- [Agent-centric wizard](./getting-started/agent-wizard.md)
- [Reproducible builds + SBOM](./getting-started/reproducibility.md)

# Recipes — additional

- [MCP server from Claude Desktop](./recipes/mcp-from-claude-desktop.md)
- [Future marketing plugin (multi-client)](./recipes/marketing-plugin-future.md)

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
[Releases](./contributing/release.md)
[License](./license.md)
[API reference (rustdoc) ↗](./api-reference.md)
