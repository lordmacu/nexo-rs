# Telegram

Bot API channel with long-polling intake, multi-bot routing, full
send/reply/reaction/edit/location/media tool surface, and optional
voice auto-transcription.

Source: `crates/plugins/telegram/`.

## Topics

| Direction | Subject | Notes |
|-----------|---------|-------|
| Inbound | `plugin.inbound.telegram` | Legacy single-bot |
| Inbound | `plugin.inbound.telegram.<instance>` | Per-bot routing |
| Outbound | `plugin.outbound.telegram` | Legacy single-bot |
| Outbound | `plugin.outbound.telegram.<instance>` | Per-bot routing |

Each instance subscribes only to its own outbound topic, so two bots
in the same process don't cross-wire.

## Config

```yaml
# config/plugins/telegram.yaml
telegram:
  token: ${file:./secrets/telegram_token.txt}
  instance: sales_bot
  polling:
    enabled: true
    interval_ms: 25000
    offset_path: ./data/media/telegram/sales_bot.offset
  allowlist:
    chat_ids: []        # empty = accept all
  auto_transcribe:
    enabled: false
    command: ./extensions/openai-whisper/target/release/openai-whisper
    language: es
  bridge_timeout_ms: 120000
```

Key fields:

| Field | Default | Purpose |
|-------|---------|---------|
| `token` | — (required) | Bot API token from @BotFather. |
| `instance` | `None` | Label for multi-bot routing. Unlabelled keeps the legacy bare topic. |
| `allow_agents` | `[]` | Agents permitted to publish from this bot. Empty = accept any agent holding a resolver handle. Defense-in-depth for the per-agent [`credentials`](../config/credentials.md) binding. |
| `polling.enabled` | `true` | Long-polling intake. Webhook not yet supported. |
| `polling.interval_ms` | `25000` | Long-poll timeout hint. Telegram clamps to [1 s, 50 s]. |
| `polling.offset_path` | `./data/media/telegram/offset` | File to persist update offset across restarts. |
| `allowlist.chat_ids` | `[]` | Numeric chat ids allowed. Empty = accept all. |
| `auto_transcribe.enabled` | `false` | Voice → text. |
| `auto_transcribe.command` | `./extensions/openai-whisper/.../openai-whisper` | Path to whisper binary. |
| `bridge_timeout_ms` | `120000` | Handler deadline before a `bridge_timeout` event fires. |

## Auth

Single mode: **static bot token**. No OAuth. Store it under
`./secrets/` and reference via `${file:...}`.

```mermaid
flowchart LR
    SETUP[agent setup] --> ASK[ask for bot token]
    ASK --> F[./secrets/telegram_token.txt]
    F -.->|${file:...}| CFG[config/plugins/telegram.yaml]
    CFG --> RUN[runtime: HTTP Bot API with long-poll]
```

## Tools exposed to the LLM

| Tool | Notes |
|------|-------|
| `telegram_send_message` | Send text to chat id (negative for groups/channels). |
| `telegram_send_reply` | Quote a specific prior message. |
| `telegram_send_reaction` | Emoji on a message. |
| `telegram_edit_message` | Modify a prior message's text. |
| `telegram_send_location` | GPS coordinates. |
| `telegram_send_media` | File upload with caption and mime hint. |

All tools enforce `outbound_allowlist.telegram` per binding.

## Event shapes

```json
// message
{
  "kind": "message",
  "from": "12345",
  "chat": "12345",
  "chat_type": "private",
  "text": "hi",
  "reply_to": null,
  "is_group": false,
  "timestamp": 1714000000,
  "msg_id": "42",
  "username": "jdoe",
  "media": [],
  "latitude": null,
  "longitude": null,
  "forward": null
}

// media item (inside `media`)
{
  "kind": "voice" | "photo" | "video" | "document" | "audio",
  "local_path": "./data/media/telegram/....ogg",
  "file_id": "AgACAgEA...",
  "mime_type": "audio/ogg",
  "duration_s": 4,
  "width": null,
  "height": null,
  "file_name": null
}

// callback_query  (inline-keyboard button press, auto-ACKed)
{"kind": "callback_query", "from": "...", "chat": "...", "data": "buy"}

// chat_membership
{"kind": "chat_membership", "chat": "...", "status": "added" | "kicked" | ...}

// lifecycle
{"kind": "connected" | "disconnected"}
{"kind": "bridge_timeout", "msg_id": "...", "waited_ms": ...}
```

Forwarded messages include a `forward` object:

```json
"forward": {
  "source": "user" | "channel" | "chat",
  "from_user_id": 12345,
  "from_chat_id": null,
  "date": 1714000000
}
```

## Gotchas

- **Webhook mode is not supported yet.** Long-polling only.
- **`polling.interval_ms` is clamped by Telegram.** Values outside
  [1000, 50000] get capped by the server side; default 25000 is a
  good middle ground.
- **Negative chat ids are groups/channels.** Telegram uses negative
  ids for group chats; positive for private. Don't strip the sign.
- **Auto-transcribe requires the whisper skill extension.** The
  command path must point at a working binary, otherwise inbound
  voice messages arrive without `text`.
