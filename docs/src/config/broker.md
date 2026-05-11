# broker.yaml

Broker topology, disk persistence, and fallback behavior.

Source: `crates/config/src/types/broker.rs`.

## Shape

```yaml
broker:
  type: nats          # nats | local
  url: nats://localhost:4222
  auth:
    enabled: false
    nkey_file: ./secrets/nats.nkey
  persistence:
    enabled: true
    path: ./data/queue
  limits:
    max_payload: 4MB
    max_pending: 10000
  fallback:
    mode: local_queue
    drain_on_reconnect: true
```

## Fields

| Field | Type | Default | Purpose |
|-------|------|---------|---------|
| `type` | `nats` \| `local` | `local` | `local` keeps the whole bus in-process; `nats` uses a real NATS server. |
| `url` | url | — | NATS connection URL (ignored when `type: local`). |
| `auth.enabled` | bool | `false` | Turn on NKey mTLS. |
| `auth.nkey_file` | path | — | Path to the NKey file when `auth.enabled`. |
| `persistence.enabled` | bool | `true` | Turn on the SQLite disk queue. |
| `persistence.path` | path | `./data/queue` | Directory for the disk queue SQLite DB. |
| `limits.max_payload` | size | `4MB` | Reject events larger than this. |
| `limits.max_pending` | u64 | `10000` | Hard cap on the disk queue; past this, oldest events are shed. |
| `fallback.mode` | `local_queue` \| `drop` | `local_queue` | What to do when NATS is unreachable. |
| `fallback.drain_on_reconnect` | bool | `true` | Replay the disk queue when NATS returns. |

## Operational notes

- **`type: local` for single-machine dev (and small prod).** You don't
  need NATS running just to try the agent. The local broker matches
  NATS subject semantics, so everything works the same.
- **Subprocess plugins work in `local` mode too (Phase 92).** When
  `type: local`, the daemon derives a `stdio_bridge` transport and
  pipes `broker.publish` / `broker.event` for the extracted
  subprocess plugins (WhatsApp, Telegram, marketing) through the
  stdio JSON-RPC channel it already uses for tool calls — no NATS
  server required. `stdio_bridge` is daemon-derived; operators never
  pick it in YAML. Full picture: [broker shapes](../architecture/broker-shapes.md).
- **Switch at runtime with `nexo set-broker`.** Rewrites this file +
  SIGHUPs the running daemon (~3 s blackout; in-flight messages
  drained from the persistence layer):
  ```bash
  nexo set-broker nats --url nats://localhost:4222   # → multi-host
  nexo set-broker local                              # → stdio bridge
  ```
- **Disk queue always on in production.** Even on a single machine.
  It's the guarantee against losing events on a NATS blip.
- **`drain_on_reconnect: true` is FIFO.** See
  [Event bus — Disk queue](../architecture/event-bus.md#disk-queue).

See also:

- [Broker shapes (local / NATS / embedded)](../architecture/broker-shapes.md)
- [Fault tolerance](../architecture/fault-tolerance.md)
- [DLQ operations](../ops/dlq.md)
