# Event subscribers

Per-agent NATS subject patterns that, when matched, fire an
agent turn. Covers the gap between webhook receivers / pollers
/ microapps publishing events and the agent runtime consuming
them. Provider-agnostic by construction.

## Quick start

```yaml
# In agents.yaml under each agent:
agents:
  - id: marketing
    event_subscribers:
      - id: github_main
        subject_pattern: "webhook.github_main.>"
        synthesize_inbound: synthesize     # synthesize | tick | off
        inbound_template: "GitHub {{event_kind}}: {{body_json.repository.full_name}} — {{body_json.action}}"
        max_concurrency: 4
        max_buffer: 64
        overflow_policy: drop-oldest       # drop-oldest | drop-newest
```

When a NATS event matches `subject_pattern`, the runtime
synthesises an inbound message and fires an agent turn. The
agent receives the rendered template (or raw JSON fallback)
in its turn context and decides what to do.

## Synthesis modes

| Mode | Behaviour |
|------|-----------|
| `synthesize` (default) | Render `inbound_template` against the event payload via mustache-lite (`{{path.to.field}}`). Fallback to JSON-stringify when no template. |
| `tick` | Fire an agent turn with a `<event subject="..." envelope_id="..."/>` marker as the body. Cheap on context window — agent can ignore or fetch payload via tooling. |
| `off` | Subscriber inactive. Useful for staging YAML before flipping it on (requires daemon restart at v0). |

## Auto-synthesised binding

When you declare an `event_subscribers` entry, the boot
supervisor automatically synthesises a matching
`inbound_bindings` entry:

```yaml
# Implied automatically from event_subscribers above:
inbound_bindings:
  - plugin: event
    instance: github_main
```

If you declare the binding manually (e.g. to override
`allowed_tools` or `sender_rate_limit` for that source), your
manual entry survives — the auto-synth is idempotent.

## Template syntax

Mustache-lite. Only `{{path.to.field}}` substitution; no
conditionals or loops.

- `{{event_kind}}` — top-level field.
- `{{body_json.action}}` — nested object access.
- `{{tags.0}}` — array index.
- Missing path → `<missing>` placeholder (does not crash).
- Object/array at the leaf → `<missing>` (avoids leaking
  struct shape into the agent body).

## Buffer + concurrency

Each binding gets its own:

- **Bounded buffer** (`max_buffer`, default 64) — absorbs bursts
  without blocking the broker.
- **Concurrency cap** (`max_concurrency`, default 1 = serial)
  — enforces ordering and limits in-flight turns.
- **Overflow policy** (`drop-oldest` default — recent events
  more relevant; `drop-newest` for conservative buffering).

Drops emit `tracing::warn!` with `binding_id` + drop counter.

## Defensive guards

- **Loop guard**: if a binding's `subject_pattern` accidentally
  matches its own re-publish topic
  (`plugin.inbound.event.<id>`), the producer drops the
  self-event with a warn — never blows the buffer.
- **`id` validation at boot**: rejects `.`, `*`, `>`, or
  whitespace in `id` (would mis-parse the re-publish topic).
- **Pattern validation**: `>`, `plugin.>`, `plugin.*.>`,
  `plugin.inbound.*` are all rejected as loop-risk patterns at
  boot.
- **Per-binding cancel token**: SIGTERM drains all subscribers
  within ≤1s.

## Worked example: GitHub webhook → marketing agent

1. Phase 82.2 webhook receiver verifies a GitHub webhook → publishes
   `webhook.github_main.pull_request` to NATS with the typed
   `WebhookEnvelope`.
2. Phase 82.4 event_subscriber matches `webhook.github_main.>` →
   renders `"GitHub pull_request: anthropic/repo — opened"` →
   re-publishes to `plugin.inbound.event.github_main`.
3. The existing inbound resolver matches the auto-synthesised
   `{ plugin: "event", instance: "github_main" }` binding →
   constructs `BindingContext` with `event_source: Some({
   subject: "webhook.github_main.pull_request",
   synthesis_mode: "synthesize", ... })`.
4. The agent fires a turn; tools see the metadata in
   `params._meta.nexo.binding.event_source`.

Microapps consuming `nexo-tool-meta` parse the `event_source`
field via `parse_binding_from_meta(args._meta).event_source`.

## Hot-reload

v0 spawns subscribers at boot only. Adding/removing
`event_subscribers` requires a daemon restart. Hot-reload via
Phase 18 reload coordinator is the deferred 82.4.c follow-up.

## Operator notes

- Subscribers are independent of the agent's session — they
  feed events into the standard inbound flow, which the runtime
  routes per session like any other inbound.
- The `_nexo_event_source` extension field on the re-published
  payload is the canonical seam between the EventSubscriber and
  the inbound resolver. Microapps should read it from the
  agent-side `_meta.nexo.binding.event_source`, not from the
  raw broker payload.
- `tracing::info!` summary at boot: look for `event subscribers
  online: count=N` to confirm wiring took.
- Validation failures are non-fatal: an invalid binding logs an
  error and skips; the daemon stays up.
