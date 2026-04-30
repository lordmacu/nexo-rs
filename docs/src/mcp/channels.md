# MCP channels — inbound surfaces from Slack / Telegram / iMessage

An **MCP channel** is any MCP server that declares the
`experimental['nexo/channel']` capability and pushes user
messages into the agent via `notifications/nexo/channel`. The
runtime treats those messages as trusted inbound: it wraps them
in `<channel source="...">…</channel>` XML and delivers them
through the same intake lane as a paired WhatsApp / Telegram /
email message.

Outbound is the mirror image: the agent invokes the server's
`send_message` tool (or the operator-configured equivalent) via
the `channel_send` LLM tool. Per-server permission relay lets a
user approve risky tools from their phone via a structured
`yes <id>` / `no <id>` reply.

This page covers the operator-facing surface. For the schema
details see `agents.channels` in the YAML reference.

## Why channels

Channels turn the agent from a thing you ask things on a
terminal into a thing that lives in the platforms your team
already uses. The same primitives that drive chat-side intake
(pairing, dispatch policy, per-binding rate limits) apply to
channel inbound — channels are not a special case for the gates
that decide whether a sender is trusted.

## YAML shape

```yaml
agents:
  - id: kate
    channels:
      enabled: true
      max_content_chars: 16000
      default_rate_limit:
        rps: 5.0
        burst: 20
      approved:
        - server: slack
          plugin_source: slack@anthropic
          outbound_tool_name: chat.postMessage
          rate_limit:
            rps: 10.0
            burst: 50
        - server: telegram
          # plugin_source omitted — accept any installed source
          # outbound_tool_name omitted — defaults to "send_message"
          # rate_limit omitted — inherits default_rate_limit
    inbound_bindings:
      - plugin: telegram
        instance: kate_tg
        allowed_channel_servers:
          - slack
          - telegram
```

## The 5-step gate

Every channel registration runs through a 5-step filter:

1. **Capability** — server declared `experimental['nexo/channel']`.
2. **Killswitch** — `agents.channels.enabled = true`. Hot
   reloadable.
3. **Per-binding session allowlist** — server name is in the
   binding's `allowed_channel_servers`.
4. **Plugin source verification** — when the approved entry
   declares `plugin_source`, the runtime's stamp must match
   exactly. Catches a malicious plugin clone with a different
   source.
5. **Approved allowlist** — server appears in
   `agents.channels.approved`. Operators can separate "binding
   may route through this server" (gate 3) from "we vetted the
   server itself" (gate 5).

Each gate emits a typed `Skip { kind, reason }` on failure so
debug output points at the exact YAML knob to fix.

## Threading

Each `(server, meta)` pair maps to a stable agent session uuid
via `ChannelSessionKey::derive`. Threading priority goes
`thread_ts` (Slack) → `thread_id` → `chat_id` (Telegram, Discord)
→ `conversation_id` → `room_id` → `channel_id` → `to`. Without
any matching key the session collapses to one per server.

The mapping persists through the SQLite-backed
`SqliteSessionRegistry` so daemon restarts don't reset Slack
threads — the bot doesn't have to re-introduce itself every
reboot.

## Outbound + permission relay

`channel_send(server, content, arguments?)` resolves the
server's outbound tool from the `RegisteredChannel` snapshot
(default `send_message`, configurable per-server) and invokes
it through the existing MCP runtime. `arguments` is passed
verbatim; `content` populates a `text` key when the operator
hasn't supplied one.

When a tool requires approval AND the agent's binding has a
channel server with `experimental['nexo/channel/permission']`,
the runtime emits `notifications/nexo/channel/permission_request`
to the server and races every channel reply against the local
prompt. The first decision wins. Reply format the server
parses and forwards as a structured event:

```
^\s*(y|yes|n|no)\s+([a-km-z]{5})\s*$
```

The 5-letter ID uses the alphabet `a-z` minus `l` (visually
confusable with `1` / `I` in many fonts). Phone autocorrect's
capitalisation of the prefix is tolerated.

## Rate limit

Per-server token bucket throttles inbound before parsing. When
the bucket is empty the message is dropped with a structured
warn — a noisy server cannot blow up memory or flood the
conversation context. Configure via `default_rate_limit` (global
ceiling) and per-server `rate_limit` (override). `0/0` means
unthrottled; the validator caps `rps` at 1000 to catch typos.

## Hot-reload

Flipping `channels.enabled` or removing a server from
`approved` triggers a re-evaluation of every active
registration via `ChannelRegistry::reevaluate`. Entries that no
longer pass the gate get unregistered with a typed
`SkipKind` reason; surviving entries stay live without a daemon
restart.

## LLM tools the agent gets

- `channel_list` — list active registrations for the agent's
  current binding (read-only, auto-approve-friendly).
- `channel_send` — outbound wrapper.
- `channel_status [server?]` — diagnostic surface (registered?
  plugin source? permission relay? registered-at-ms?). When
  `server` is omitted, returns one row per registered server.

All three resolve `binding_id` from `ctx.effective.binding_index`
at call time, falling back to `agent_id` for paths without a
binding match.

## Audit

Every turn driven by a channel inbound writes
`source: "channel:<server>"` into the Phase 72 turn-log
(`goal_turns` table). Operators can answer "what came in via
Slack today?" with a single SQL filter on the indexed `source`
column.

## See also

- [Channel doctor (operator CLI)](../ops/channel-doctor.md)
- [Concept — pairing](../config/pairing.md) — channel inbound
  flows through the same pairing gate as WhatsApp / Telegram
  inbound, so a sender that hasn't been allowlisted will see
  a `[pairing]` denial just like any other surface.
