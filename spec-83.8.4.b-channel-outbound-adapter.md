# Spec — 83.8.4.b production `ChannelOutboundDispatcher` adapter

**Status:** spec (post `/forge brainstorm 83.8.4.b`).
Brainstorm: `proyecto/brainstorm-83.8.4.b-channel-outbound-adapter.md`.

## 1. Mining (regla irrompible)

### research/

- `research/extensions/whatsapp/src/connection-controller.ts:20` —
  TS WhatsApp send shape (single-process, in-memory). Confirms
  the per-channel translator approach: every channel has a
  bespoke send signature; one universal payload would lose
  fidelity. Rust diverge — uses NATS broker + per-plugin
  dispatcher loop.
- `research/src/channels/conversation-binding-context.ts:1` —
  binding-id render shape. Already mirrored in Rust by Phase
  82.1 `BindingContext`.

### claude-code-leak/

**Ausente.** `ls /home/familia/chat/claude-code-leak/` retorna
"No such file or directory". Cumplido el reporte por la regla.

### In-repo (la fuente de verdad)

- `crates/plugins/whatsapp/src/dispatch.rs:31` — topic
  `plugin.outbound.whatsapp` and `OutboundPayload` shape `{to,
  text, kind, msg_id, emoji, url, caption, file_name}`.
- `crates/plugins/whatsapp/src/pairing_adapter.rs:25` — per-account
  topic suffix convention `plugin.outbound.whatsapp.<account>`.
- `crates/plugins/telegram/src/tool.rs:50-62` — telegram analog
  (`plugin.outbound.telegram[.<account>]`).
- `crates/plugins/email/src/plugin.rs:20` — email constant
  `TOPIC_OUTBOUND = "plugin.outbound.email"`.
- `crates/broker/src/handle.rs:25` —
  `BrokerHandle::publish(topic, event) -> Result<(), BrokerError>`.
- `crates/broker/src/types.rs::Event::new(topic, source, payload)` —
  envelope constructor used elsewhere in the workspace.

## 2. Architecture

```
┌──────────────┐
│ Operator UI  │
└──────┬───────┘
       │ POST /tools/takeover_send
       ▼
┌──────────────────┐
│ agent-creator    │  ToolCtx::admin().call(
│ microapp tool    │    "nexo/admin/processing/intervention", ...)
└──────┬───────────┘
       │ stdio JSON-RPC
       ▼
┌──────────────────────────────────────────┐
│ daemon admin RPC dispatcher              │
│  → processing::intervention              │
│    → ChannelOutboundDispatcher::send     │ ◄── trait (Phase 83.8.4.a)
└──────────────────────┬───────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────┐
│ BrokerOutboundDispatcher (NEW — 83.8.4.b)│
│   match msg.channel:                     │
│     "whatsapp" → WhatsAppTranslator      │
│     "telegram" → TelegramTranslator      │
│     "email"    → EmailTranslator         │
│     other      → ChannelUnavailable       │
│                                          │
│   broker.publish(topic, payload)         │
└──────────────────────┬───────────────────┘
                       │ NATS / local broker
                       ▼
┌──────────────────────────────────────────┐
│ plugin outbound dispatcher (existing)    │
│   crates/plugins/whatsapp/src/dispatch.rs│
│   subscribed to plugin.outbound.whatsapp │
│   → Session::send_text(to, text)         │
└──────────────────────┬───────────────────┘
                       │
                       ▼
                   WhatsApp wire
```

## 3. Wire shapes (none new)

`OutboundMessage` already exists from Phase 83.8.4.a:

```rust
pub struct OutboundMessage {
    pub channel: String,
    pub account_id: String,
    pub to: String,
    pub body: String,
    pub msg_kind: String,
    pub attachments: Vec<Value>,
    pub reply_to_msg_id: Option<String>,
}
```

This spec adds adapter implementations that translate it into
each plugin's existing payload shape — zero new admin RPC types.

## 4. New types

### 4.1 — `ChannelPayloadTranslator` trait

**Crate:** `crates/setup/src/admin_adapters.rs` (extend).

```rust
/// Per-plugin translator that knows the broker topic + JSON
/// payload shape its plugin's existing outbound dispatcher
/// expects. Lives in `nexo-setup` so each plugin crate stays
/// independent of the trait (the adapter's wires are setup-only
/// concerns).
pub trait ChannelPayloadTranslator: Send + Sync + std::fmt::Debug {
    /// Channel name this translator serves (e.g. `"whatsapp"`).
    /// Used at `BrokerOutboundDispatcher` lookup time.
    fn channel(&self) -> &str;

    /// Translate the operator-driven `OutboundMessage` into the
    /// per-plugin `(topic, payload)` pair. Errors land at the
    /// admin RPC layer as `-32602 invalid_params`.
    fn translate(
        &self,
        msg: &OutboundMessage,
    ) -> Result<(String, serde_json::Value), TranslationError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TranslationError {
    #[error("missing field: {0}")]
    MissingField(&'static str),
    #[error("invalid value for {field}: {reason}")]
    InvalidValue { field: &'static str, reason: String },
    #[error("unsupported msg_kind: {0}")]
    UnsupportedKind(String),
}
```

### 4.2 — `BrokerOutboundDispatcher` struct

```rust
pub struct BrokerOutboundDispatcher {
    broker: AnyBroker,
    translators: HashMap<String, Box<dyn ChannelPayloadTranslator>>,
}

impl BrokerOutboundDispatcher {
    pub fn new(broker: AnyBroker) -> Self { ... }

    pub fn with_translator(
        mut self,
        translator: Box<dyn ChannelPayloadTranslator>,
    ) -> Self { ... }
}

#[async_trait]
impl ChannelOutboundDispatcher for BrokerOutboundDispatcher {
    async fn send(
        &self,
        msg: OutboundMessage,
    ) -> Result<OutboundAck, ChannelOutboundError> { ... }
}
```

### 4.3 — `WhatsAppTranslator`

```rust
#[derive(Debug, Default)]
pub struct WhatsAppTranslator;

impl ChannelPayloadTranslator for WhatsAppTranslator {
    fn channel(&self) -> &str { "whatsapp" }

    fn translate(
        &self,
        msg: &OutboundMessage,
    ) -> Result<(String, Value), TranslationError> {
        let topic = if msg.account_id.is_empty()
            || msg.account_id == "default"
        {
            "plugin.outbound.whatsapp".to_string()
        } else {
            format!("plugin.outbound.whatsapp.{}", msg.account_id)
        };
        let payload = match msg.msg_kind.as_str() {
            "text" => json!({
                "to": msg.to,
                "text": msg.body,
                "kind": "text",
            }),
            "reply" => {
                let msg_id = msg
                    .reply_to_msg_id
                    .as_deref()
                    .ok_or(TranslationError::MissingField("reply_to_msg_id"))?;
                json!({
                    "kind": "reply",
                    "name": "reply",
                    "payload": { "to": msg.to, "msg_id": msg_id, "text": msg.body },
                })
            }
            "media" => {
                let url = msg.attachments
                    .first()
                    .and_then(|a| a.get("url"))
                    .and_then(Value::as_str)
                    .ok_or(TranslationError::MissingField("attachments[0].url"))?;
                json!({
                    "kind": "media",
                    "to": msg.to,
                    "url": url,
                    "caption": msg.body,
                    "file_name": msg.attachments[0].get("file_name").cloned(),
                })
            }
            other => return Err(TranslationError::UnsupportedKind(other.into())),
        };
        Ok((topic, payload))
    }
}
```

### 4.4 — Telegram + Email translators (deferred)

Out of scope for this sub-fase. Microapp v1 is WhatsApp-only
(memory `feedback_only_whatsapp_channel.md`). Each translator
becomes a separate sub-fase when its channel is ramped:

- `83.8.4.b.tg` — `TelegramTranslator` (chat_id parsing,
  parse_mode handling).
- `83.8.4.b.em` — `EmailTranslator` (subject/body split, per-instance
  topic suffix).

Until then `BrokerOutboundDispatcher` returns
`ChannelOutboundError::ChannelUnavailable` for those channel
names, which surfaces in admin RPC as
`-32603 internal: channel_unavailable: <ch>`.

## 5. Boot wiring

### 5.1 — `AdminBootstrapInputs` extension

**File:** `crates/setup/src/admin_bootstrap.rs`.

Add the broker handle the bootstrap needs to construct the
adapter:

```rust
pub struct AdminBootstrapInputs<'a> {
    // existing fields …
    /// Phase 83.8.4.b — broker handle the
    /// `BrokerOutboundDispatcher` publishes to. `None` keeps the
    /// outbound surface disabled — `processing.intervention`
    /// returns `-32603 channel_unavailable`. Production wiring
    /// in `main.rs` always passes `Some(broker.clone())`.
    pub broker: Option<AnyBroker>,
}
```

### 5.2 — Dispatcher wiring

In `AdminRpcBootstrap::build_inner`, after the existing
`with_*_domain` chain, add:

```rust
if let Some(broker) = inputs.broker.clone() {
    let outbound = Arc::new(
        BrokerOutboundDispatcher::new(broker)
            .with_translator(Box::new(WhatsAppTranslator)),
    );
    dispatcher = dispatcher.with_channel_outbound(outbound);
}
```

When `inputs.broker` is `None` (older test paths), the
`processing.intervention` handler still returns the typed
`channel_outbound dispatcher not configured` error so callers
diagnose the wire-up gap clearly.

### 5.3 — `main.rs` change

The daemon's `main.rs` already constructs `AnyBroker`. Pass it
into `AdminBootstrapInputs`:

```rust
inputs.broker = Some(broker.clone());
```

## 6. Error mapping

| `TranslationError` | `ChannelOutboundError` | JSON-RPC |
|---|---|---|
| `MissingField(_)` | `InvalidParams(_)` | `-32602` |
| `InvalidValue { .. }` | `InvalidParams(_)` | `-32602` |
| `UnsupportedKind(_)` | `InvalidParams(_)` | `-32602` |

| `BrokerError` | `ChannelOutboundError` | JSON-RPC |
|---|---|---|
| publish failed | `Transport(_)` | `-32603 internal: transport: …` |

| Unknown channel | `ChannelUnavailable(_)` | `-32603 internal: channel_unavailable: …` |

## 7. End-to-end flow

```
1. Operator UI → POST /tools/takeover_send {
       scope: { kind: "conversation", agent_id, channel: "whatsapp",
                account_id: "wa.0", contact_id: "wa.55" },
       operator_token_hash: "abc",
       channel: "whatsapp",
       account_id: "wa.0",
       to: "wa.55",
       body: "Hola, soy un humano",
       msg_kind: "text"
   }

2. Microapp tool → admin.processing.intervention(...)

3. processing::intervention → ChannelOutboundDispatcher::send(OutboundMessage{...})

4. BrokerOutboundDispatcher.send:
   - lookup translators["whatsapp"] → WhatsAppTranslator
   - translate → ("plugin.outbound.whatsapp.wa.0",
                   { "to": "wa.55", "text": "Hola...", "kind": "text" })
   - broker.publish(topic, Event::new(topic, "whatsapp", payload))
   - return Ok(OutboundAck { outbound_message_id: None })

5. processing::intervention → ProcessingAck { changed: true, correlation_id }

6. plugin/whatsapp/dispatch.rs (existing subscriber):
   - parse OutboundPayload from event
   - call Session::send_text("wa.55", "Hola...")
   - bytes go out via wa-rs

7. End user receives message on WhatsApp.
```

## 8. Decisions (closed-form)

| # | Decisión | Razón |
|---|---|---|
| D1 | Adapter publishes to broker, no plugin code call | Plugins ya tienen dispatchers subscribed; reusa la infra y mantiene microservicios. |
| D2 | Per-channel translator trait | WhatsApp / Telegram / Email payloads divergen materialmente; uno universal pierde info. |
| D3 | Translators viven en `nexo-setup` | Cycle-free: setup ya depende de los plugins; plugins no dependen del trait core. |
| D4 | v1 ships WhatsApp translator only | Microapp scope = whatsapp-only (regla `feedback_only_whatsapp_channel.md`). Telegram/email = follow-up. |
| D5 | `outbound_message_id = None` v0 | Plugin dispatchers fire-and-forget; correlation ack es follow-up. |
| D6 | Topic con account-suffix opcional | Topic = `plugin.outbound.<ch>[.<account>]`; coincide con wiring existente plugin-side. |
| D7 | `account_id == ""` ó `"default"` → topic base | Single-account daemons no necesitan suffix; matchea convention plugin actual. |
| D8 | `msg_kind = "reply"` reusa shape Custom Reply existente | WhatsApp `dispatch.rs::OutboundPayload::Custom { name: "reply", payload: { to, msg_id, text } }` ya soporta esto — no inventar shape nueva. |
| D9 | `inputs.broker = None` → outbound desconectado, no boot fail | Tests + dev daemons sin canales pueden seguir corriendo. processing.intervention returns Internal error. |
| D10 | Sin pre-flight check de subscriber count | NATS no expone esta información de forma uniforme; v0 deja silently-dropped al broker layer. Audit log captura el dispatch. Follow-up: sub-fase de health-check. |

## 9. Risks

- **R1 — Account ID format mismatch**: WhatsApp plugin dispatch
  uses `account_id_raw()` derived from auth handle. The
  `OutboundMessage.account_id` arrives from operator UI via
  scope. Mitigation: spec mandates the UI passes the same
  string the daemon already exposes via `nexo/admin/agents/get`
  → `inbound_bindings[].account_id`. Bonus: integration test
  asserts a real WhatsApp pairing's account_id round-trips
  through translate.
- **R2 — Multi-message send**: takeover may want to send 2-3
  consecutive messages. Adapter handles them as N independent
  `intervention` calls — no batching. Spec confirms one-call
  one-message.
- **R3 — Broker offline**: NATS down → `BrokerError::Publish`.
  Phase 18 fault tolerance (local-mpsc fallback + drain on
  reconnect) already covers this; adapter just propagates the
  error.
- **R4 — Translator unsupported `msg_kind`**: WhatsApp v1 honors
  `text`, `reply`, `media`. Anything else surfaces as
  `-32602 invalid_params: unsupported msg_kind: <kind>`.
  Microapp UI validates before sending.
- **R5 — Topic typo**: hard-coded constant `plugin.outbound.whatsapp`
  is repeated in plugin dispatch + adapter translator. Risk of
  drift. Mitigation: re-export the constant from
  `crates/plugins/whatsapp/src/dispatch.rs` and import it in
  `WhatsAppTranslator`. Telegram/email follow the same pattern.

## 10. Out of scope

- `outbound_message_id` correlation ack flow.
- Telegram + Email translators (their own follow-up sub-fases).
- Health probe / subscriber count check.
- Outbound rate limiting per agent (Phase 82.7 already covers
  per-binding tool rate limits; this is on the *operator*
  side which has its own implicit rate via UI).
- Audit log enrichment with `outbound_message_id` (it's `None`
  v0 so no benefit yet).

## 11. Done criteria

- ✅ `BrokerOutboundDispatcher` + `ChannelPayloadTranslator` +
  `WhatsAppTranslator` shipped in `crates/setup/src/admin_adapters.rs`.
- ✅ `TranslationError` enum with three variants + `From` impl.
- ✅ `AdminBootstrapInputs.broker: Option<AnyBroker>` field.
- ✅ Boot wiring in `build_inner` constructs the adapter when
  `Some(broker)` is set.
- ✅ `main.rs` passes the daemon's broker handle.
- ✅ Tests:
  - Unit: WhatsAppTranslator translates `text` / `reply` / `media`
    and rejects unknown `msg_kind`.
  - Unit: BrokerOutboundDispatcher routes by channel name and
    returns `ChannelUnavailable` for unknown channels.
  - Integration: in-process LocalBroker, takeover_send →
    intervention → adapter publish → fake subscriber asserts
    payload shape end-to-end.
- ✅ `cargo build --workspace` clean + `cargo test --workspace`
  passing.
- ✅ FOLLOWUPS.md `83.8.4.b` entry moved to "Resolved".
- ✅ Telegram + Email translators logged as separate
  sub-fases.

---

**Listo para `/forge plan 83.8.4.b-channel-outbound-adapter`.**
