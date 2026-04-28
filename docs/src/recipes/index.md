# Recipes

End-to-end walkthroughs that wire multiple subsystems together.
Each recipe runs against a clean checkout of nexo-rs — prerequisites
are at the top.

| Recipe | What you build |
|--------|----------------|
| [WhatsApp sales agent](./whatsapp-sales-agent.md) | A drop-in agent that greets WhatsApp leads, asks qualifying questions, and notifies a human on hot leads. |
| [Agent-to-agent delegation](./agent-to-agent.md) | Route work from one agent to another using `agent.route.*` with correlation ids. |
| [Python extension](./python-extension.md) | Write a stdlib-only extension that adds a custom tool to any agent. |
| [MCP server from Claude Desktop](./mcp-from-claude-desktop.md) | Expose the agent's tools to the Anthropic desktop client. |
| [NATS with TLS + auth](./nats-tls-auth.md) | Harden the broker for a multi-node deployment. |
| [Rotating config without downtime](./hot-reload.md) | Three Phase 18 hot-reload scenarios: API key rotation, A/B prompt swap, narrowing an outbound allowlist mid-incident. |
| [Future marketing plugin (multi-client)](./marketing-plugin-future.md) | Prepare multi-client autonomous marketing agents with strict instance/model isolation before plugin implementation. |

If a recipe drifts from reality, open an issue — it means the docs
didn't get updated alongside a code change.
