# Channel doctor

`nexo channel` is an operator CLI for debugging the MCP-channels
surface without a running daemon. Three verbs:

```text
nexo channel list   [--config=<path>] [--json]
nexo channel doctor [--config=<path>] [--binding=<id>] [--json]
nexo channel test   <server> [--binding=<id>] [--content=...]
                    [--config=<path>] [--json]
```

All three read from the operator's YAML directly. They never
spin up the daemon, never connect to a live MCP server, and
never publish on the broker. Safe to run on production
configs from any operator workstation.

## `nexo channel list`

Walks every agent and surfaces `(enabled, approved_servers,
bindings)` per agent. When `--json` is passed the output is
machine-readable; otherwise the renderer groups by agent for
human reading.

```bash
$ nexo channel list
## agent kate — channels.ENABLED (2 approved)
  approved: slack
  approved: telegram
  binding telegram:kate_tg: 2 server(s) — slack, telegram
```

When an agent has no `channels.approved` entries the
`(no approved servers)` placeholder makes the gap obvious. When
no binding lists `allowed_channel_servers`, `(no binding has
allowed_channel_servers)` highlights the configuration is
incomplete.

## `nexo channel doctor`

Runs the **static half** of the 5-step gate against every
`(agent, binding, server)` triple in the YAML. The doctor
cannot probe a live MCP server, so gate 1 (capability declared)
is *assumed* true; gates 2/3/5 run normally; gate 4 (plugin
source) reads from the approved entry. Each row carries one of
three outcomes:

- `WOULD REGISTER` — every static gate passes; the only thing
  the live daemon will check is whether the server actually
  declares the capability.
- `SKIP { kind, reason }` — typed reason. `disabled` =
  `channels.enabled: false`. `session` = binding doesn't list
  the server. `marketplace` = `plugin_source` mismatch.
  `allowlist` = server isn't in `approved`.
- `NOT BOUND` — the server appears in `approved` but no binding
  lists it. Surfaces a half-configured state where the operator
  vetted the server but forgot to bind it.

Filter to one binding with `--binding=<plugin>:<instance>`. The
binding id format mirrors what the runtime registers — the same
string that shows up in agent logs.

```bash
$ nexo channel doctor --binding=telegram:kate_tg
| Agent | Binding            | Server   | Outcome        | Skip       | Reason |
|-------|--------------------|----------|----------------|------------|--------|
| kate  | telegram:kate_tg   | slack    | WOULD REGISTER | -          | all static gates pass; live runtime must declare the capability |
| kate  | telegram:kate_tg   | telegram | WOULD REGISTER | -          | all static gates pass; live runtime must declare the capability |
```

## `nexo channel test`

Synthesises a `notifications/nexo/channel` payload (with sample
`chat_id` and `user` meta) and runs it through
`parse_channel_notification` + `wrap_channel_message`. Prints
the model-facing `<channel>` block plus the derived
`session_key`. Cheap dry-run for tuning meta-key whitelists or
verifying content-cap behaviour.

```bash
$ nexo channel test slack
# Channel test — server=slack

session_key: slack|chat_id=C_TEST

--- rendered XML (model-facing) ---
<channel source="slack" chat_id="C_TEST" user="operator">
hello from slack — channel test payload
</channel>
```

Override the body with `--content="..."` to test how the
content cap (`agents.channels.max_content_chars`) clips long
payloads. The output flags `[content truncated by
max_content_chars]` when the cap fired.

## When to use which

- **Setting up channels for the first time** → `list` to verify
  the YAML structure, then `doctor` to confirm the gate would
  let the binding register, then start the daemon.
- **A server stopped delivering messages** → `doctor` to see if
  the gate would still register it. Common causes:
  `channels.enabled` flipped off; binding's
  `allowed_channel_servers` doesn't include the server (typo);
  approved entry got renamed.
- **Tuning meta-key whitelists / content caps** → `test
  <server>` with various `--content` payloads.

## Live-runtime checks

`doctor` is intentionally static. To check live state — what's
actually registered in the running daemon — the agent calls
`channel_list` / `channel_status` from inside a turn, or the
operator inspects the `mcp.channel.>` NATS subjects directly.
Live-runtime CLI is on the roadmap.

## See also

- [MCP channels concept](../mcp/channels.md) — the full picture
  including threading, permission relay, and the hot-reload
  re-evaluation pass.
