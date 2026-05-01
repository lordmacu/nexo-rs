# Summary

[Introduction](./introduction.md)
[Install for AI assistants](./install-for-ai.md)

# Getting started

- [Installation](./getting-started/installation.md)
- [Native install (no Docker)](./getting-started/install-native.md)
- [Debian / Ubuntu (.deb)](./getting-started/install-deb.md)
- [Fedora / RHEL / Rocky (.rpm)](./getting-started/install-rpm.md)
- [Termux (Android)](./getting-started/install-termux.md)
- [Nix flake](./getting-started/install-nix.md)
- [Quick start](./getting-started/quickstart.md)
- [Setup wizard](./getting-started/setup-wizard.md)
- [Agent-centric wizard](./getting-started/agent-wizard.md)
- [Verifying releases](./getting-started/verify.md)
- [Reproducible builds + SBOM](./getting-started/reproducibility.md)

# Architecture

- [Overview](./architecture/overview.md)
- [Agent runtime](./architecture/agent-runtime.md)
- [Event bus (NATS)](./architecture/event-bus.md)
- [Fault tolerance](./architecture/fault-tolerance.md)
- [Transcripts (FTS + redaction)](./architecture/transcripts.md)
- [vs OpenClaw](./architecture/vs-openclaw.md)
- [Driver subsystem (Phase 67)](./architecture/driver-subsystem.md)
- [Project tracker + dispatch (Phase 67.A–H)](./architecture/project-tracker.md)
- [Plan mode (Phase 79.1)](./architecture/plan-mode.md)
- [TodoWrite scratch list (Phase 79.4)](./architecture/todo-write.md)
- [ToolSearch deferred schemas (Phase 79.2)](./architecture/tool-search.md)
- [SyntheticOutput typed-output (Phase 79.3)](./architecture/synthetic-output.md)
- [NotebookEdit Jupyter cells (Phase 79.13)](./architecture/notebook-edit.md)
- [RemoteTrigger outbound publisher (Phase 79.8)](./architecture/remote-trigger.md)
- [REPL stateful sandbox (Phase 79.12)](./architecture/repl.md)
- [LSP tool (Phase 79.5)](./architecture/lsp.md)
- [Cron schedule tools (Phase 79.7)](./architecture/cron-schedule.md)
- [MCP router tools (Phase 79.11)](./architecture/mcp-router-tools.md)
- [ConfigTool gated self-config (Phase 79.10)](./architecture/config-tool.md)
- [Team tools (Phase 79.6)](./architecture/team-tools.md)
- [MCP server exposable catalog (Phase 79.M)](./architecture/mcp-server-exposable.md)
- [Fork subagent infra (Phase 80.19)](./architecture/fork-subagent.md)

# Configuration

- [Layout](./config/layout.md)
- [agents.yaml](./config/agents.md)
- [llm.yaml](./config/llm.md)
- [broker.yaml](./config/broker.md)
- [memory.yaml](./config/memory.md)
- [Drop-in agents](./config/drop-in.md)
- [Per-agent credentials](./config/credentials.md)
- [Pollers (pollers.yaml)](./config/pollers.md)

# LLM providers

- [MiniMax M2.5](./llm/minimax.md)
- [Anthropic / Claude](./llm/anthropic.md)
- [OpenAI-compatible](./llm/openai.md)
- [DeepSeek](./llm/deepseek.md)
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
- [1Password](./extensions/onepassword.md)

# Microapps

- [Building microapps in Rust](./microapps/rust.md)

# MCP

- [Introduction](./mcp/introduction.md)
- [Client (stdio / HTTP)](./mcp/client.md)
- [Agent as MCP server](./mcp/server.md)
- [Channels (Slack / Telegram / iMessage inbound)](./mcp/channels.md)
- [HTTP+SSE transport](./extensions/mcp-server.md)
- [Building an MCP server extension](./extensions/mcp-server-extension.md)

# Skills

- [Catalog](./skills/catalog.md)
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

# CLI

- [Reference](./cli/reference.md)
- [Migrations CLI (status)](./cli/migrations.md)
- [Background agents (`agent run --bg` / ps / attach)](./cli/agent-bg.md)

# Operations

- [Proactive mode](./agents/proactive-mode.md)
- [Docker](./ops/docker.md)
- [Metrics (Prometheus)](./ops/metrics.md)
- [Logging](./ops/logging.md)
- [DLQ](./ops/dlq.md)
- [Config hot-reload](./ops/hot-reload.md)
- [Compact tiers](./ops/compact-tiers.md)
- [Memdir scanner](./ops/memdir-scanner.md)
- [Bash safety knobs](./ops/bash-safety.md)
- [Capability toggles](./ops/capabilities.md)
- [Channel doctor (MCP)](./ops/channel-doctor.md)
- [Webhook receiver](./ops/webhook-receiver.md)
- [Event subscribers](./ops/events.md)
- [Per-binding tool rate-limits](./ops/per-binding-rate-limits.md)
- [Context optimization](./ops/context-optimization.md)
- [Link understanding](./ops/link-understanding.md)
- [Web search](./ops/web-search.md)
- [Web fetch](./ops/web-fetch.md)
- [Pairing](./ops/pairing.md)
- [Telemetry (opt-in)](./ops/telemetry.md)
- [Benchmarks](./ops/benchmarks.md)
- [Backup + restore](./ops/backup.md)
- [Memory snapshots](./ops/memory-snapshot.md)
- [Privacy toolkit (GDPR)](./ops/privacy.md)
- [Health checks](./ops/health.md)
- [Cost & quota](./ops/cost.md)

# Recipes

- [Index](./recipes/index.md)
- [WhatsApp sales agent](./recipes/whatsapp-sales-agent.md)
- [Agent-to-agent delegation](./recipes/agent-to-agent.md)
- [Python extension](./recipes/python-extension.md)
- [MCP server from Claude Desktop](./recipes/mcp-from-claude-desktop.md)
- [NATS with TLS + auth](./recipes/nats-tls-auth.md)
- [Rotating config without downtime](./recipes/hot-reload.md)
- [Future marketing plugin (multi-client)](./recipes/marketing-plugin-future.md)
- [Build a poller module](./recipes/build-a-poller.md)
- [Deploy on Hetzner Cloud](./recipes/deploy-hetzner.md)
- [Deploy on Fly.io](./recipes/deploy-fly.md)
- [Deploy on AWS (EC2)](./recipes/deploy-aws.md)

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
