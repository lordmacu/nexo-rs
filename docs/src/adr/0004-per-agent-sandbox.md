# ADR 0004 — Per-agent tool sandboxing at registry build time

**Status:** Accepted
**Date:** 2026-02

## Context

The same process hosts agents with **very different** blast radii.
Ana runs on WhatsApp against leads; Kate manages a personal Telegram;
`ops` has Proxmox credentials. The LLM in one agent must never see —
let alone invoke — tools registered for another agent.

Three enforcement points are possible:

1. **Prompt-level sandboxing** — "don't use these tools." Relies on
   model compliance. Fails under adversarial prompts.
2. **Runtime filter** — every `tools/call` checks a policy before
   dispatch. Robust, but the LLM still sees the tools in
   `tools/list` and can hallucinate calls.
3. **Registry build-time pruning** — the agent's `ToolRegistry` is
   built with only the allowed tools. The LLM literally cannot see
   the others.

## Decision

**Default to registry build-time pruning.**

- `allowed_tools: []` (empty) = every registered tool visible
- `allowed_tools: [glob, …]` = strict allowlist, tools not matching
  are **removed from the registry** before the LLM's `tools/list`
  call is answered
- For agents with `inbound_bindings[]`, the base registry keeps every
  tool and **per-binding overrides** apply build-time filtering at
  turn time — a single agent can narrow its surface differently per
  channel

Additional layers stack on top:

- `outbound_allowlist.<channel>: [recipients]` — even with
  `whatsapp_send_message` in the registry, the runtime rejects sends
  to unlisted recipients (defense in depth)
- `tool_rate_limits` — per-tool rate limiting for side-effectful
  tools
- Per-agent `workspace` and `long-term memory (WHERE agent_id = ?)` —
  data-level isolation

## Consequences

**Positive**

- Adversarial prompts can't invoke missing tools — the model has no
  token string for them
- Easy mental model: grep `allowed_tools` to see what an agent can
  do
- Prompt tokens stay small (tool list scales with allowlist, not
  registry)

**Negative**

- A misconfigured `allowed_tools` silently hides tools the LLM
  expected to use — the agent returns "I can't do that," puzzling
  both user and developer. Mitigation: `agent status` shows the
  effective tool set per agent
- Dynamic granting mid-session is not supported (would require
  re-handshake with the MCP clients)

## Related

- [Config — agents.yaml (allowed_tools semantics)](../config/agents.md#tool-sandboxing)
- [Per-agent credentials](../config/credentials.md) — the gauntlet
  validates that the binding's channel instance is actually allowed
