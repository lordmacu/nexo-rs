# wa-agent Integration ADR

**Crate:** `wa-agent` on crates.io / `whatsapp_rs` import name. Owned by us
(lordmacu/whatsapp-rs). Rust library with an agent-oriented runtime layer
on top of a WhatsApp multi-device client.

Audited at commit corresponding to `wa-agent = 0.1.0` (local path
`/home/familia/whatsapp-rs`). This doc is the reference the rest of
Phase 6 builds on.

---

## Why this matters

`wa-agent` ships with most of what a production WhatsApp bot needs
already solved: outbox, reconnect with backoff+jitter, liveness watchdog,
NewMessage dedup, token-bucket rate limiter (global + per-JID), typing
heartbeat, slow-notice heartbeat, chat-meta auto-skip, ACL, voice-note
transcription hook, on-disk state store. Phase 6 must integrate these —
not re-implement them.

## Public API we depend on

### Bootstrap

| Symbol | Signature | Use |
|--------|-----------|-----|
| `Client::new()` | `Result<Client>` | Builds a client, reads `XDG_DATA_HOME/.whatsapp-rs/` |
| `Client::connect()` | `async -> Result<Session>` | Pairs (QR on first run) or resumes |
| `Session::our_jid` | `String` | Self JID once connected |

### Agent loop

| Symbol | Signature | Use |
|--------|-----------|-----|
| `Session::run_agent_with` | `async (Acl, F) -> Result<()>` where `F: Fn(AgentCtx) -> impl Future<Output = Response>` | Primary inbound entry |
| `Session::run_agent_with_transcribe` | `async (Acl, T: Transcriber, F) -> Result<()>` | Inbound with STT hook |
| `agent::AgentCtx { msg, text }` | struct | Handler input |
| `agent::Response` | enum `Noop / Text / Reply / React / Image / Video / Multi` | Handler output |
| `agent::Acl::{open, from_env, allow}` | builder | JID allow-list |
| `agent::Transcriber` | trait | Voice-note hook |

### Direct send (proactive path)

| Symbol | Signature | Command mapping |
|--------|-----------|-----------------|
| `Session::send_text(jid, text)` | `async -> Result<String>` | `Command::SendMessage` |
| `Session::send_reply(jid, reply_to_id, text)` | `async -> Result<String>` | `Custom { name: "reply" }` |
| `Session::send_reaction(jid, target_id, emoji)` | `async -> Result<()>` | `Custom { name: "react" }` |
| `Session::send_image(jid, &[u8], Option<&str>)` | `async -> Result<String>` | `Command::SendMedia` (image mime) |
| `Session::send_video(jid, &[u8], Option<&str>)` | `async -> Result<String>` | `Command::SendMedia` (video mime) |
| `Session::send_audio(jid, &[u8], mimetype)` | `async -> Result<String>` | `Command::SendMedia` (audio/*) |
| `Session::send_voice_note(jid, &[u8], mimetype)` | `async -> Result<String>` | `Custom { name: "voice" }` |
| `Session::send_document(jid, &[u8], mimetype, file_name)` | `async -> Result<String>` | `Command::SendMedia` (application/*) |
| `Session::send_presence(bool)` | `async -> Result<()>` | Custom / not routed |
| `Session::download_media(info, media_type)` | `async -> Result<Vec<u8>>` | Inbound media pull |
| `Session::download_media_by_id(jid, msg_id)` | `async -> Result<Vec<u8>>` | Inbound media pull by key |
| `Session::outbox_pending_count()` | `async -> usize` | Health |
| `Session::outbox_pending()` | `async -> Vec<(String, WAMessage)>` | Health / metrics |

### Event types seen from `Session::events()` (broadcast)

`MessageEvent::{Connected, Disconnected, Reconnecting, NewMessage,
Receipt, MessageUpdate, Reaction, Typing, Presence, MessageRevoke,
MessageEdit, EphemeralSetting, GroupUpdate, PollVote, HistorySync,
AppStateUpdate}`.

The handler form via `run_agent_with` only surfaces `NewMessage` (after
filtering `from_me`, ACL, chat-meta). Other events remain available via
`Session::events()` for lifecycle wiring (6.6 health + QR re-emit).

---

## Concurrency semantics of `run_agent_with`

Verified in `src/agent.rs::run_agent_full`: the loop is a **single task
awaiting `rx.recv()` and calling `handler(ctx).await` inline**. Messages
are therefore processed **strictly serially across all chats** within
one `Session`.

Implications for the plugin:

1. A slow handler (LLM latency) blocks every other chat's handler.
2. The wa-agent typing heartbeat ticks for the duration of `handler`,
   which is exactly what we want user-facing.
3. We must keep the handler short if we want cross-chat concurrency.

**Decision:** For v1 (personal assistant, low throughput) we accept the
serial model — the typing indicator feels "native". If throughput
becomes a blocker, a follow-up spawns the broker publish + oneshot-await
in `tokio::spawn` and returns `Response::Noop` immediately, sending the
actual reply via the proactive path. This is an orthogonal optimisation.

---

## Daemon vs in-process

`wa-agent` ships a daemon mode (`whatsapp-rs` binary + systemd unit)
that keeps a single session alive and accepts JSON IPC over a **TCP
loopback socket**, not a Unix socket. Port + token are advertised in
`$XDG_DATA_HOME/.whatsapp-rs/daemon.json`. Daemon mode drops per-command
latency from ~2.5s to <100ms and survives agent-process crashes.

### ADR-001: in-process `Session` for Phase 6

- **Decision:** The plugin owns its own `Session` via `Client::new`
  + `connect`. No daemon.
- **Why:**
  - Single process = simpler Docker (`docker-compose up` and go).
  - No port/firewall/token ceremony.
  - wa-agent's in-process robustness (outbox, reconnect) already
    covers most of what the daemon would insulate us from.
  - WhatsApp allows only one primary socket per device; running both
    daemon and in-process at once loses the session. Cleaner to pick
    one.
- **Collision guard:** on boot, the plugin checks
  `$XDG_DATA_HOME/.whatsapp-rs/daemon.json` and aborts with a clear
  message when `daemon.prefer_existing: true` and the file exists.
  This prevents accidentally opening a second socket against the same
  account.
- **Follow-up:** a daemon-backed Extension (Phase 11) can reuse the
  wa-agent daemon transparently — same `Command`/`Response` contract
  over IPC. Out of scope for Phase 6.

---

## Credentials / state layout

`Client::new` respects `XDG_DATA_HOME`. We override it at plugin-boot
time so each agent installation isolates its own WA state under
`cfg.session_dir`:

```
cfg.session_dir/
  .whatsapp-rs/
    creds.json           # device identity + pre-keys root
    sessions.json        # Signal double-ratchet sessions
    pre-keys/            # batch of pre-keys
    messages/            # rolling message store (LLM history)
    outbox.jsonl         # pending sends, persisted
    scheduled.json       # cron-like entries (unused by us)
    contacts.json
    chat_meta.json       # muted/archived/locked projection
    app-state/           # app-state sync cache
    daemon.json          # only present when daemon is running
```

Delete `creds.json` → forces re-pair on next boot. We back it up as
`creds.json.bak.{timestamp}` before any destructive recovery so a
mis-detected corruption doesn't permanently kick the account.

---

## Command ↔ wa-agent mapping

```
SendMessage { to, text }          → Session::send_text(to, text)
Custom { "reply" }                → Session::send_reply(to, reply_to_id, text)
Custom { "react" }                → Session::send_reaction(chat, msg_id, emoji)
Custom { "typing" }               → Session::send_presence / typing API
Custom { "voice" }                → Session::send_voice_note(to, bytes, mime)
SendMedia { to, url, caption }    → download(url) + sniff(mime) → one of:
                                     send_image / send_video /
                                     send_audio / send_document
```

All of the above inherit outbox persistence — if WhatsApp is momentarily
disconnected, the send is queued and drained on reconnect.

---

## Bridge design (Model C)

1. **Inbound (reactive):** Plugin's `run_agent_with` handler receives
   `AgentCtx`. Derives `session_id = UUIDv5(NAMESPACE_WA, bare_jid)`.
   Inserts a `oneshot::Sender<String>` into `PendingMap[session_id]`,
   publishes `plugin.inbound.whatsapp` on the broker with
   `Event.session_id = Some(sid)` + text/from/chat/is_group payload,
   awaits the oneshot with `cfg.bridge.response_timeout_ms` (default
   30s). On resolve → `Response::Text(reply)`. On timeout →
   `cfg.bridge.on_timeout` policy (`Noop` or fixed apology text).
2. **Outbound (reactive or proactive):** A second task subscribes to
   `plugin.outbound.whatsapp`. For each event, if `event.session_id`
   matches a pending oneshot, the reply is delivered via the oneshot
   (so wa-agent renders it in-handler as the `Response`, keeping the
   typing indicator natural). Otherwise (proactive: heartbeat
   reminders, A2A delegations), call `Session::send_text` directly —
   still outbox-backed.

This uses `Event.session_id`, already propagated by core
(`llm_behavior.rs:402-409`). No protocol change needed in core.

---

## Version pin plan

- **Phase 6 development:** `wa-agent = { path = "../../../whatsapp-rs" }`
  — we iterate both repos in parallel, avoid `cargo publish` on every
  shared change.
- **Phase 6 merge/lock:** switch to `wa-agent = "=0.1.x"` with the
  lowest version that contains every symbol in the API table above.
  Exact pin (`=`) because the crate is pre-1.0 and reserves the right
  to break on any minor bump.
- **Post-1.0:** move to a caret range (`^x.y`) once the crate commits
  to semver.

---

## Things deliberately NOT used

- `wa-agent::agent::Router` — routing lives in the LLM system prompt.
- `wa-agent::scheduler` — we already have Heartbeat (Phase 7) +
  ReminderTool. Using both would double-fire.
- `wa-agent::webhook::run_webhook_agent` — the broker is our webhook.
- `wa-agent::agent::StateStore` — short-term memory lives in core
  (`crates/memory`); no split-brain.
- Polls / buttons / list / status stories — out of scope (follow-up).

---

## Risks flagged for the rest of Phase 6

| # | Risk | Addressed in |
|---|------|--------------|
| R1 | Serial handler blocks other chats | Document + follow-up switch to spawn-and-proactive if needed |
| R2 | Session dir collision with an existing daemon | 6.6 boot check on `daemon.json` |
| R3 | `creds.json` corruption on disk | 6.6 timestamped backup before re-pair |
| R4 | Bridge oneshot timeout leaves user without reply | 6.3 `on_timeout` policy + `BridgeTimeout` observability event |
| R5 | LLM reply arrives after oneshot overwritten by a new inbound | Accepted — core debounces multiple inbound into one reply; the latest oneshot "wins", user sees one unified answer |
| R6 | wa-agent 0.x API churn | Path dep during Phase 6; exact-pin at end |
