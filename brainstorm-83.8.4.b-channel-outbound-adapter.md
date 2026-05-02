# Brainstorm — 83.8.4.b production `ChannelOutboundDispatcher` adapter

**Status:** brainstorm. Sub-fase derivada de FOLLOWUPS.md
"Phase 83.8.4.b — production ChannelOutboundDispatcher adapter".

## 1. Problema

Phase 83.8.4.a shipped:

- `ChannelOutboundDispatcher` trait en `crates/core/src/agent/admin_rpc/channel_outbound.rs` — channel-agnostic.
- `processing.intervention` handler ya invoca el trait con `OutboundMessage { channel, account_id, to, body, msg_kind, attachments, reply_to_msg_id }`.
- Boot wiring expone `with_channel_outbound(Arc<dyn ChannelOutboundDispatcher>)`.

Lo que **falta** es la implementación que conecta este trait con los plugins reales. Hoy el dispatcher no está wired en `setup/admin_bootstrap.rs`, así que `takeover_send` desde la microapp retorna:

```
Internal: channel_outbound dispatcher not configured
```

## 2. Mining (regla irrompible)

### research/

- `research/extensions/wacli/backend.ts:1` — pattern de outbound en TS:
  el plugin posee su propio canal de envío. Limitación: en TS no hay
  separación clara entre "core invoca outbound" y "plugin posee outbound";
  el dispatcher se llama en proceso. **No copiar** — Rust microservicios
  separa via NATS.
- `research/extensions/whatsapp/src/connection-controller.ts:1` —
  shape ergonómico de `send_text` / `send_media`. Equivalente Rust
  ya existe vía `whatsapp_rs::Session::send_text/send_image/...`.

### claude-code-leak/

**Ausente.** `ls /home/familia/chat/claude-code-leak/` retorna "No such file or directory". Cumplido el reporte por la regla.

### Existing in-repo code (la fuente de verdad)

Los plugins **ya tienen** outbound dispatchers escuchando NATS:

| Plugin | Topic | Payload schema |
|---|---|---|
| WhatsApp | `plugin.outbound.whatsapp[.<account>]` | `{to, text, kind, msg_id, emoji, url, caption, file_name}` (ver `crates/plugins/whatsapp/src/dispatch.rs:40-60`) |
| Telegram | `plugin.outbound.telegram[.<account>]` | shape per-bot via `tool.rs:50-62` |
| Email | `plugin.outbound.email.<instance>` | shape SMTP via `outbound.rs:1-50` |
| Browser | `plugin.outbound.browser` | CDP commands |

**Insight clave**: el adapter NO llama código de plugin. Solo publica al
broker en el topic correcto + el plugin existente hace el resto.
Reduce el adapter a ~50 LOC + tests, no a refactor de N plugins.

## 3. Diseño propuesto

### 3.1 — `BrokerOutboundDispatcher` adapter

**Crate:** `crates/setup/src/admin_adapters.rs` (extender — ya tiene
los otros adapters production).

**Struct:**

```rust
pub struct BrokerOutboundDispatcher {
    broker: AnyBroker,
    /// Per-channel translator that knows its plugin's payload
    /// schema. WhatsApp/Telegram/Email differ enough that one
    /// universal shape would lose information. Boxed so each
    /// translator stays independent.
    translators: HashMap<String, Box<dyn ChannelPayloadTranslator>>,
}

#[async_trait]
trait ChannelPayloadTranslator: Send + Sync {
    /// Returns (topic, payload) for the broker publish call.
    /// Errors propagate as ChannelOutboundError::InvalidParams.
    fn translate(&self, msg: &OutboundMessage) -> Result<(String, Value), String>;
}
```

### 3.2 — Translator implementations

- `WhatsAppTranslator` → topic `plugin.outbound.whatsapp[.{account_id}]`,
  payload `{to, text: body, kind: msg_kind, ...}`.
- `TelegramTranslator` → topic `plugin.outbound.telegram[.{account_id}]`,
  payload `{chat_id: to.parse::<i64>()?, text: body}`.
- `EmailTranslator` → topic `plugin.outbound.email.{account_id}`,
  payload `{to, subject: ?, body, ...}` — email needs more fields,
  may require richer `OutboundMessage` extension OR a `attachments`
  parsing convention.

### 3.3 — `impl ChannelOutboundDispatcher for BrokerOutboundDispatcher`

```rust
async fn send(&self, msg: OutboundMessage)
    -> Result<OutboundAck, ChannelOutboundError>
{
    let translator = self.translators
        .get(&msg.channel)
        .ok_or_else(|| ChannelOutboundError::ChannelUnavailable(msg.channel.clone()))?;
    let (topic, payload) = translator.translate(&msg)
        .map_err(ChannelOutboundError::InvalidParams)?;
    self.broker.publish(&topic, Event::new(&topic, &msg.channel, payload))
        .await
        .map_err(|e| ChannelOutboundError::Transport(e.to_string()))?;
    // Plugin dispatchers are fire-and-forget over the broker;
    // they don't ack synchronously. outbound_message_id stays
    // None for v1; a future correlation_id wire-back can add it.
    Ok(OutboundAck { outbound_message_id: None })
}
```

### 3.4 — Boot wiring

`crates/setup/src/admin_bootstrap.rs`:

```rust
let outbound = Arc::new(
    BrokerOutboundDispatcher::new(broker.clone())
        .with_translator("whatsapp", Box::new(WhatsAppTranslator))
        .with_translator("telegram", Box::new(TelegramTranslator))
        .with_translator("email", Box::new(EmailTranslator)),
);
dispatcher.with_channel_outbound(outbound);
```

## 4. Decisiones (closed-form)

| # | Decisión | Razón |
|---|---|---|
| D1 | Adapter = broker publish, no llamada directa a plugin | Plugins ya tienen dispatcher subscribed; reusa la infra existente y mantiene el modelo microservicios. |
| D2 | Per-channel translator trait, no JSON one-size-fits-all | WhatsApp/Telegram/Email tienen payloads materially distintos. Translator preserva el schema cada plugin entiende. |
| D3 | `outbound_message_id = None` para v1 | Plugins son fire-and-forget broker. Correlation back-flow es follow-up (broker reply pattern). |
| D4 | v1 incluye solo WhatsApp translator | Microapp v1 scope = whatsapp-only (memory `feedback_only_whatsapp_channel.md`). Telegram/email translators = follow-up cuando se agreguen. |
| D5 | Translators viven en `nexo-setup`, no en cada plugin crate | Cycle-free: setup ya depende de todos los plugins; plugins no deberían depender de un trait core. |
| D6 | Topic con account-suffix opcional | Si `msg.account_id` está set y no es default → topic `plugin.outbound.<ch>.<account>`; sino → topic base. Coincide con el wiring que los plugins ya implementan. |

## 5. Out of scope

- **outbound_message_id correlation**: requiere broker reply pattern + ack listener. Follow-up `83.8.4.c`.
- **Telegram + Email translators**: cuando se ramp esos canales (regla `feedback_only_whatsapp_channel.md`).
- **Audit log integration**: el dispatch ya se loggea via Phase 82.10.h SQLite audit (entrada por dispatch); no necesita cambios.
- **Retry policy / circuit breaker**: el broker layer ya tiene Phase 18 fault tolerance (NATS offline → local fallback). No re-implementar acá.

## 6. Casos de uso end-to-end

```
[Operador UI] → POST /tools/takeover_send { scope, body, channel: "whatsapp", account_id, to }
  → microapp tool → ctx.admin().call("nexo/admin/processing/intervention", ...)
    → daemon dispatcher → processing::intervention
      → ChannelOutboundDispatcher::send(OutboundMessage)
        → BrokerOutboundDispatcher
          → WhatsAppTranslator.translate → ("plugin.outbound.whatsapp.<acct>", {to, text, kind})
          → broker.publish
        → Ok(OutboundAck { None })
      → ProcessingAck { changed: true }
  ← microapp tool reply

[whatsapp plugin dispatch loop] receives broker event
  → Session::send_text(to, text)
  → bytes go out via wa-rs
  → user's WhatsApp receives the message
```

## 7. Riesgos / open questions

- **R1 — Per-account topic resolution:** plugin code uses
  `account_id_raw()` from auth handle to render the suffix.
  Adapter receives `account_id` as plain `String` from
  `OutboundMessage`. Risk: format mismatch (e.g. plugin expects
  `wa.123456@s.whatsapp.net` but admin RPC ships `123456`).
  Mitigation: spec defines the canonical account id format
  consumed by `OutboundMessage.account_id`; translator
  passes-through verbatim.
- **R2 — Plugin not running:** adapter publishes to topic but
  no subscriber consumes → message lost silently. Mitigation:
  add a `pre-flight` check (broker has at least one subscriber
  on the topic). Defer to follow-up; v1 just logs.
- **R3 — `msg_kind = "media"` payload:** WhatsApp translator
  needs `url + caption + file_name` from `attachments[]`.
  Currently `OutboundMessage.attachments: Vec<Value>` is
  free-form. Spec must lock: `attachments[0]` for media, with
  `{url, caption?, file_name?}` shape.
- **R4 — Reply-to:** WhatsApp `Custom { reply, ... }` payload
  uses `msg_id`. Translator maps `OutboundMessage.reply_to_msg_id`
  to that field when present. Verify with existing
  `dispatch.rs::OutboundPayload`.
- **R5 — Broker not connected:** `setup/admin_bootstrap.rs`
  needs the broker handle. Currently the dispatcher is built
  before broker init in some paths. Mitigation: defer
  `with_channel_outbound` wire to *after* broker init in main.rs
  bootstrap order.

## 8. Cortes propuestos para spec

```
83.8.4.b.1  Wire shape — ChannelPayloadTranslator trait + WhatsAppTranslator + BrokerOutboundDispatcher
83.8.4.b.2  Boot wire — admin_bootstrap.rs constructs + injects via with_channel_outbound
83.8.4.b.3  Integration test — end-to-end takeover_send through real broker fake whatsapp subscriber
```

3 commits. Bounded.

---

**Listo para `/forge spec 83.8.4.b-channel-outbound-adapter`.**
