# Plugin patterns

Common shapes for nexo subprocess plugins. Each pattern is a
template you adapt — pick the closest match to what you're
building, copy the skeleton, modify.

All patterns work in any of the 4 SDK languages (Rust / Python /
TypeScript / PHP). Examples below use the language that's
clearest for the pattern.

---

## Pattern 1 · Echo channel

> When to use · You're learning the SDK or wiring a brand-new
> channel and want a smoke-test before adding logic.

The plugin echoes every inbound `broker.event` back as
`broker.publish` on a mirrored topic. Useful for verifying the
wire format end-to-end before you write business logic.

```python
from nexo_plugin_sdk import PluginAdapter, Event

async def on_event(topic, event, broker):
    out_topic = topic.replace("plugin.outbound.", "plugin.inbound.")
    await broker.publish(out_topic, Event.new(out_topic, "my_plugin", event.payload))

await PluginAdapter(manifest_toml=MANIFEST, on_event=on_event).run()
```

→ Used in every template (`extensions/template-plugin-{rust,python,typescript,php}/`)

---

## Pattern 2 · Webhook receiver

> When to use · An external service POSTs JSON; you want the
> daemon to see it as a `plugin.inbound.<kind>` event.

Plugin runs an HTTP server (or listens on a Unix socket) for
inbound POST requests. Each request becomes a broker publish.
Plugin's manifest declares an `http_server` capability so the
daemon's reverse-proxy / port-allocator wires the route.

```rust
use nexo_microapp_sdk::plugin::{BrokerSender, Event, PluginAdapter};
use axum::{Router, routing::post, extract::State, Json};

async fn webhook(State(broker): State<Arc<BrokerSender>>, Json(body): Json<Value>) {
    let event = Event::new("plugin.inbound.webhook", "my_plugin", body);
    let _ = broker.publish("plugin.inbound.webhook", event).await;
}

#[tokio::main]
async fn main() -> Result<()> {
    let adapter = PluginAdapter::new(MANIFEST);
    let broker = adapter.broker();
    tokio::spawn(async move {
        let app = Router::new().route("/webhook", post(webhook)).with_state(broker);
        axum::serve(listener, app).await
    });
    adapter.run().await
}
```

Manifest declares the inbound topic the plugin will publish to:

```toml
[[plugin.channels.register]]
kind = "webhook"
adapter = "WebhookAdapter"
```

---

## Pattern 3 · RPC bridge to an external API

> When to use · You're exposing a third-party service (Stripe,
> Twilio, internal CRM) as a tool the agent can call.

Plugin doesn't deal with channels — it registers as a tool
provider. The agent sends a `tool.call` request; the plugin
forwards to the external API and replies.

```typescript
import { PluginAdapter } from "nexo-plugin-sdk";

const adapter = new PluginAdapter({
  manifestToml: MANIFEST,
  onEvent: async (topic, event, broker) => {
    if (topic === "plugin.tool.stripe.create_invoice") {
      const inv = await stripeClient.invoices.create(event.payload);
      await broker.publish("plugin.tool.stripe.create_invoice.reply",
        Event.new("plugin.tool.reply", "stripe-bridge", { result: inv }));
    }
  },
});
```

Manifest contributes the tool:

```toml
[[plugin.tools.expose]]
name = "stripe.create_invoice"
schema_path = "./tools/create_invoice.json"
```

---

## Pattern 4 · Scheduled poller

> When to use · You need to poll an external feed every N minutes
> and publish only changes (deltas) to the broker.

Plugin holds local state (last-seen IDs / etag / cursor),
re-polls on a timer, dedupes against state, publishes new items.
Persist state to `<state_dir>/<plugin_id>/state.json` so restarts
don't re-emit historical items.

```python
import asyncio, json, aiohttp
from pathlib import Path
from nexo_plugin_sdk import PluginAdapter, Event

STATE = Path(".nexo-state/poller.json")
seen_ids: set[str] = set(json.loads(STATE.read_text())) if STATE.exists() else set()

async def poll_loop(broker):
    while True:
        async with aiohttp.ClientSession() as s:
            items = await (await s.get("https://example.com/feed.json")).json()
        for item in items:
            if item["id"] in seen_ids:
                continue
            seen_ids.add(item["id"])
            await broker.publish("plugin.inbound.feed",
                Event.new("plugin.inbound.feed", "feed_poller", item))
        STATE.write_text(json.dumps(list(seen_ids)))
        await asyncio.sleep(300)  # 5 min

adapter = PluginAdapter(manifest_toml=MANIFEST)
asyncio.create_task(poll_loop(adapter.broker))
await adapter.run()
```

→ See [Build a poller module](../recipes/build-a-poller.md) for the YAML-only path that doesn't need a plugin at all.

---

## Pattern 5 · Long-running connection (websocket / SSE)

> When to use · The external service is push-based (Slack RTM,
> Discord gateway, MQTT broker, custom WebSocket).

Plugin opens the persistent connection at startup. Inbound
messages from the external side become `broker.publish` events.
On disconnect, the plugin reconnects with exponential backoff.

```rust
use tokio_tungstenite::connect_async;

let (ws, _) = connect_async("wss://gateway.discord.gg/").await?;
let (write, mut read) = ws.split();
// Auth handshake omitted...

while let Some(msg) = read.next().await {
    let evt = parse_discord(msg?)?;
    broker.publish("plugin.inbound.discord", evt).await?;
}
// On disconnect: reconnect with backoff.
```

The SDK's signal handling (default-on) lets the daemon shut the
plugin down cleanly even mid-connection.

---

## Pattern 6 · Stateful conversation glue

> When to use · The external channel sends fragments (audio chunks,
> typing indicators, partial messages) and you want to assemble
> them before the agent sees a complete event.

Plugin maintains a per-conversation buffer; only emits a
`broker.publish` when the message is "complete" (final chunk,
silence timeout, or explicit done marker).

```python
buffer: dict[str, list[str]] = {}
timers: dict[str, asyncio.Task] = {}

async def on_chunk(conv_id, fragment, broker):
    buffer.setdefault(conv_id, []).append(fragment)
    if conv_id in timers:
        timers[conv_id].cancel()
    timers[conv_id] = asyncio.create_task(flush_after(conv_id, broker, delay=2.0))

async def flush_after(conv_id, broker, delay):
    await asyncio.sleep(delay)
    text = "".join(buffer.pop(conv_id, []))
    await broker.publish("plugin.inbound.assembled",
        Event.new("plugin.inbound.assembled", "voice_glue", {"text": text}))
```

---

## Pattern 7 · Outbound-only adapter

> When to use · The plugin only sends (Twilio SMS sender, push
> notification dispatcher, Slack outbound webhook).

Plugin subscribes to `plugin.outbound.<kind>` events from the
daemon, calls the external API, and publishes a `delivery_status`
event back so the agent knows whether it landed.

```typescript
const adapter = new PluginAdapter({
  manifestToml: MANIFEST,
  onEvent: async (topic, event, broker) => {
    if (topic.startsWith("plugin.outbound.sms")) {
      const result = await twilio.messages.create({
        to: event.payload.to,
        body: event.payload.body,
        from: TWILIO_FROM,
      });
      await broker.publish("plugin.delivery.sms",
        Event.new("plugin.delivery.sms", "twilio-out",
                  { sid: result.sid, status: result.status }));
    }
  },
});
```

---

## Pattern 8 · Provider abstraction (multi-instance)

> When to use · Operator wants 3 different Telegram bots, each
> isolated. Or 5 WhatsApp accounts.

Plugin manifest declares `instance` support. Operator's config
spawns N copies, each with a distinct `instance` label. Topics
become `plugin.inbound.<kind>.<instance>` so agent bindings can
target a specific one.

```yaml
# operator's pollers.yaml
plugins:
  telegram:
    - instance: support-bot
      bot_token_env: TG_SUPPORT_TOKEN
    - instance: sales-bot
      bot_token_env: TG_SALES_TOKEN
```

The plugin reads `instance` from `args` or env at startup and
publishes to `plugin.inbound.telegram.<instance>`.

---

## Choosing between patterns

| If you... | Use |
|-----------|-----|
| Are wiring a brand-new channel for the first time | Echo (pattern 1) |
| Need to receive HTTP from an external service | Webhook receiver (2) |
| Are exposing an external API as a tool | RPC bridge (3) |
| Need to poll something on a timer | Scheduled poller (4) |
| Have a push-based external service | Long-running connection (5) |
| Receive fragmented inputs (chunks, partials) | Stateful glue (6) |
| Only need to send (no receive) | Outbound-only (7) |
| Want N copies of the same plugin | Provider abstraction (8) |

---

## See also

- [Plugin contract](./contract.md) — the wire format every plugin
  implements.
- [Publishing a plugin](./publishing.md) — CI workflow + asset
  convention.
- [Python SDK](./python-sdk.md), [TypeScript SDK](./typescript-sdk.md), [PHP SDK](./php-sdk.md) — language-specific guides.
