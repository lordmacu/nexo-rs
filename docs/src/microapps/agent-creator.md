# `agent-creator` — SaaS meta-microapp (Phase 83.8)

A reference microapp that drives the framework as a **multi-tenant
SaaS meta-creator of WhatsApp agents**. Operators (the SaaS owner)
provision one daemon per company; clients (tenants) CRUD their own
agents, skills, LLM keys, and conversation views through the
microapp.

Lives **out of the workspace** at
`/home/familia/chat/agent-creator-microapp/`. Pulls
`nexo-microapp-sdk` + `nexo-tool-meta` + `nexo-compliance-primitives`
via path deps during dev; switch to crates.io once published.

## Tool surface (22 tools)

### Agents — Phase 83.8.8

| Tool | Backed by |
|---|---|
| `agent_list` | `nexo/admin/agents/list` |
| `agent_get` | `nexo/admin/agents/get` |
| `agent_upsert` | `nexo/admin/agents/upsert` |
| `agent_delete` | `nexo/admin/agents/delete` |

### Skills — Phase 83.8.8

| Tool | Backed by |
|---|---|
| `skill_list` | `nexo/admin/skills/list` |
| `skill_get` | `nexo/admin/skills/get` |
| `skill_upsert` | `nexo/admin/skills/upsert` |
| `skill_delete` | `nexo/admin/skills/delete` |

The skill body lands at `<root>/<name>/SKILL.md` — the runtime
`SkillLoader` reads it on every agent turn, so a CRUD round-trip
shows up in the agent's prompt without a daemon restart.

### LLM providers — Phase 83.8.8

| Tool | Backed by |
|---|---|
| `llm_provider_list` | `nexo/admin/llm_providers/list` |
| `llm_provider_upsert` | `nexo/admin/llm_providers/upsert` |
| `llm_provider_delete` | `nexo/admin/llm_providers/delete` |

### Pairing — Phase 83.8.9

| Tool | Backed by |
|---|---|
| `whatsapp_pair_start` | `nexo/admin/pairing/start` |
| `whatsapp_pair_status` | `nexo/admin/pairing/status` |
| `whatsapp_pair_cancel` | `nexo/admin/pairing/cancel` |

### Conversations — Phase 83.8.9

| Tool | Backed by |
|---|---|
| `conversation_list` | `nexo/admin/agent_events/list` |
| `conversation_read` | `nexo/admin/agent_events/read` |
| `conversation_search` | `nexo/admin/agent_events/search` |

The live firehose (`nexo/notify/agent_event`) is consumed by the
SDK `TranscriptStream::filter_by_agent` helper — multi-tenant
defense-in-depth drops events whose `agent_id` is not in the
tenant's allowed set before the microapp ever sees the frame.

### Operator takeover — Phase 83.8.9

| Tool | Backed by |
|---|---|
| `takeover_engage` | SDK `HumanTakeover::engage` → `nexo/admin/processing/pause` |
| `takeover_send` | `HumanTakeover::send_reply` → `nexo/admin/processing/intervention` |
| `takeover_release` | `HumanTakeover::release` → `nexo/admin/processing/resume` |

`takeover_send` flows operator-typed replies through the
`ChannelOutboundDispatcher` trait wired in Phase 83.8.4.a — Phase
83.8.4.b ships the production
`BrokerOutboundDispatcher` (`nexo_setup::admin_adapters`) that
publishes to the per-channel
`plugin.outbound.<channel>[.<account>]` topic each plugin's
existing dispatcher already listens on. WhatsApp translator
ships in v1; Telegram + Email translators are TBD per-channel
follow-ups (`83.8.4.b.tg` / `83.8.4.b.em`).

### Escalations — Phase 83.8.9

| Tool | Backed by |
|---|---|
| `escalation_list` | `nexo/admin/escalations/list` |
| `escalation_resolve` | `nexo/admin/escalations/resolve` |

`EscalationReason::UnknownQuery` (Phase 83.8.5) covers the
"agent doesn't know" UI notification path.

## Compliance hook — Phase 83.8.10

The `before_message` hook chains:

1. `OptOutMatcher` (Spanish + English keywords) → `Abort`.
2. `AntiLoopDetector` (3 repetitions in 60 s) → `Abort`.
3. `PiiRedactor` (cards / phones / emails) → log redaction stats.

Defaults-on. Per-agent override propagation through
`extensions_config.compliance` is logged in `FOLLOWUPS.md` as a
framework follow-up — needs the wire shape on `BindingContext`.

## Capabilities (`plugin.toml`)

```toml
[capabilities.admin]
required = [
    "agents_crud", "skills_crud", "llm_keys_crud",
    "pairing_initiate", "transcripts_read",
    "operator_intervention",
    "escalations_read", "escalations_resolve",
]
optional = ["credentials_crud", "channels_crud"]
```

The operator grants these in
`extensions.yaml.<id>.capabilities_grant`. Missing required →
boot-time fail-fast; missing optional → handler-time `-32004`.

## SDK opt-in

```rust
Microapp::new(APP_NAME, env!("CARGO_PKG_VERSION"))
    .with_admin()                       // Phase 83.8.8.a
    .with_hook("before_message", hooks::compliance::before_message)
    .with_tool("agent_list", tools::agents::agent_list)
    // … 21 more tools
    .run_stdio()
    .await
```

`with_admin()` wires the SDK `AdminClient` through the same
stdout writer the daemon-reply path uses, intercepts inbound
`app:` correlation IDs, and exposes the client through
`ToolCtx::admin()` / `HookCtx::admin()`. Tool handlers do **no**
hand-rolled JSON-RPC plumbing — every admin call is one
`ctx.admin()?.call("nexo/admin/<method>", &params).await`.

## Stress-test methodology

This microapp exists to stress-test the framework. Friction
encountered during construction triggers a framework fix
(agnostic + reusable by other microapps), not a microapp-side
workaround. Five gaps closed during the v1 build:

1. `nexo/admin/skills/*` CRUD missing → end-to-end shipped.
2. `processing.intervention` did not dispatch outbound →
   `ChannelOutboundDispatcher` trait + handler wire.
3. SDK `AdminClient` had no runtime integration →
   `Microapp::with_admin()` builder + ToolCtx accessor.
4. Operator UI needed `EscalationReason::UnknownQuery` →
   variant added.
5. SDK lacked `HumanTakeover` + `TranscriptStream::filter_by_agent`
   helpers → both shipped.

See [`FOLLOWUPS.md`](../../../FOLLOWUPS.md) for the active
deferred-follow-up list.
