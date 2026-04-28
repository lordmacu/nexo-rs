# Future marketing plugin: multi-client autonomous operation

This recipe shows how the current runtime can operate with a future
`marketing` plugin without changing core architecture.

## Goal

Run multiple marketing clients in the same system while preserving:

- strict client isolation by plugin instance
- per-agent model isolation (no cross-token usage)
- autonomous review loops with operator interrupts

## 1. Agent template

Start from:

- `config/agents.d/marketing.multiclient.example.yaml`

The template maps one client surface to one agent via strict bindings:

- `marketing_acme_intake` listens only to `plugin.inbound.marketing.acme_inbox`
- `marketing_bravo_retention` listens only to `plugin.inbound.marketing.bravo_retention`
- `marketing_charlie_exec` listens only to `plugin.inbound.marketing.charlie_exec`

Each agent has its own `model.provider` + `model.model`.

## 2. Future plugin event contract

Your future plugin should publish inbound events with this shape:

- topic: `plugin.inbound.marketing.<instance>`
- `Event.session_id`: deterministic UUID per conversation thread
- payload fields:
  - `text` (required for text turns)
  - `from` (sender/account/contact id)
  - `priority` (optional: `now | next | later`)
  - optional metadata (`channel`, `campaign_id`, `thread_id`, etc.)

Minimal example payload:

```json
{
  "text": "Customer asked to pause campaign due to legal review.",
  "from": "client-ops@acme.com",
  "priority": "next"
}
```

Urgent interrupt payload:

```json
{
  "text": "STOP ALL SENDS NOW",
  "from": "head-of-marketing@acme.com",
  "priority": "now"
}
```

## 3. Practical flow with current runtime logic

1. The plugin publishes on `plugin.inbound.marketing.acme_inbox`.
2. Runtime matches `inbound_bindings` strictly by `(plugin, instance)`.
3. `marketing_acme_intake` receives the event; other agents do not.
4. Turn runs with ACME agent model only (`MiniMax-M2.5` in template).
5. If the agent chooses `Sleep`, runtime schedules proactive wake and
   injects a synthetic `<tick>` later.
6. If `priority: now` arrives during an in-flight turn, runtime preempts
   the current turn and handles the urgent message first.
7. A BRAVO event on `plugin.inbound.marketing.bravo_retention` runs on
   the BRAVO agent model only (`claude-haiku-4-5` in template).

## 4. Why this is already ready for production hardening

- Queue priority is built-in (`now > next > later`) with in-flight preemption.
- Proactive loop is built-in (`Sleep` + wake `<tick>` + daily budget).
- Session isolation is built-in (per-session debounce task).
- Binding isolation is built-in (strict plugin/instance matching).
- Model isolation is built-in (resolved from the matched agent/binding policy).

## 5. Rollout checklist when plugin is built

1. Emit deterministic `session_id` per thread.
2. Publish to instance-scoped inbound topics (`plugin.inbound.marketing.<instance>`).
3. Send `priority: now` only for real interrupts.
4. Keep one agent per client surface when you need strict token/cost isolation.
5. Narrow `allowed_tools` once the marketing tool surface is finalized.
