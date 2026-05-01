# sample-channel-server

A minimal MCP server that acts as a channel surface for nexo
(Phase 80.9). Use it to validate end-to-end the channel
pipeline without spinning up a real Slack / Telegram / iMessage
adapter.

## What it does

- Speaks JSON-RPC over stdio, line-delimited frames.
- Declares `experimental['nexo/channel']` and
  `experimental['nexo/channel/permission']` at `initialize`.
- Exposes one tool `send_message` that echoes the input — nexo's
  `channel_send` LLM tool will hit this for outbound replies.
- Periodically emits fake `notifications/nexo/channel`
  notifications (default every 30 s) so the agent sees inbound
  messages flow through the bridge.
- Auto-approves any `notifications/nexo/channel/permission_request`
  it receives (after a short delay) — exercises the
  permission-relay race against the local prompt.

## Wire it up

Drop the following into your nexo config:

`config/mcp.yaml`:

```yaml
mcp:
  enabled: true
  servers:
    sample-channel:
      kind: stdio
      command: cargo
      args:
        - run
        - --quiet
        - --bin
        - sample-channel-server
      cwd: /path/to/nexo-rs
```

`config/agents.yaml` (excerpt):

```yaml
agents:
  - id: kate
    channels:
      enabled: true
      max_content_chars: 4000
      approved:
        - server: sample-channel
    inbound_bindings:
      - plugin: telegram
        instance: kate_tg
        allowed_channel_servers:
          - sample-channel
```

Then:

```bash
nexo channel doctor          # static sanity-check before starting
nexo run --config config/    # daemon picks up the server
```

Within a minute you should see in the agent's transcript:

```xml
<channel source="sample-channel" chat_id="C_SAMPLE" thread_ts="1.001" user="sample">
Fake message #1 from sample. If you see this in the agent's
transcript, the channel pipeline works end-to-end.
</channel>
```

## Environment knobs

All optional — sane defaults work for casual testing.

| Variable | Default | What it does |
|----------|---------|-------------|
| `NEXO_SAMPLE_CHANNEL_INTERVAL_SECS` | `30` | Interval between fake inbound emissions. `0` disables. |
| `NEXO_SAMPLE_CHANNEL_AUTO_APPROVE` | `1` | `1` to auto-approve permission requests, `0` to ignore them (lets the local prompt win every race). |
| `NEXO_SAMPLE_CHANNEL_PERMISSION_DELAY_MS` | `500` | Milliseconds before auto-approving — adjust to test race timing. |
| `NEXO_SAMPLE_CHANNEL_NAME` | `sample` | Rendered into `meta.user` for fake inbounds. |
| `RUST_LOG` | (env-driven) | Tracing filter — set `info` for default. |

## What this is NOT

- Production-ready. There is no audit trail, no real human in
  the loop on permission prompts, and the auto-approval logic
  trusts every `request_id` it receives.
- A reference implementation of a real channel adapter. Look at
  `extensions/template-mcp-server/` for the production-ready
  builder pattern. This file uses hand-rolled JSON-RPC because
  the channel surface needs to *emit* notifications outbound
  (not just respond to incoming requests), which the
  `McpServerBuilder` doesn't expose.

## Verifying the wire

If channels aren't flowing:

```bash
nexo channel doctor                     # YAML sanity
nexo channel test sample-channel        # synth a parse + wrap
RUST_LOG=info nexo run --config config/ # daemon with channel logs
```

The daemon emits `tracing::info!(server, binding, "channel
inbound loop running")` once per `(binding, server)` registered
post-MCP-handshake. If you don't see that log line, gate 1
(capability declared) is the most likely culprit — make sure
the server actually emits the `experimental['nexo/channel']`
block in its `initialize` reply (`RUST_LOG=trace cargo run --bin
sample-channel-server` shows the boot logs from this fixture).
