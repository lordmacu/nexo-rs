# Broker deployment shapes

The nexo daemon supports three legitimate broker deployment shapes,
covering single-host dev / production server clusters / embedded
mobile builds with a single `broker.yaml` switch + (in some cases)
a different build mode. Phase 92's stdio-bridge closed the gap that
forced operators to install NATS for any single-host deployment.

## Picking a shape

```
┌────────────────────────────────────────────────────────────┐
│  Multiple daemons across hosts?                            │
│                                                            │
│   YES ──→ Shape 1: NATS                                    │
│           broker.yaml type: nats                           │
│           Plugins: subprocess (extracted out-of-tree)      │
│           Infra:   NATS server / cluster on the network    │
│                                                            │
│   NO  ──→ Single host. Mobile / embedded?                  │
│                                                            │
│           YES (Android / iOS / WASM / Flutter FFI)         │
│            ──→ Shape 3: Embedded                           │
│                broker.yaml type: local                     │
│                Plugins: lib-linked Rust crates (no spawn)  │
│                Infra:   none                               │
│                                                            │
│           NO (laptop dev / server deb / desktop app)       │
│            ──→ Shape 2: Server single-host                 │
│                broker.yaml type: local                     │
│                Plugins: subprocess (extracted)             │
│                Infra:   none — stdio-bridge handles xprocess│
│                                                            │
└────────────────────────────────────────────────────────────┘
```

## Shape 1 — Server multi-host (NATS)

The classical setup. NATS server runs on the network; every daemon
in the cluster connects to it. Subprocess plugins connect directly
to the same NATS, addressed by URL.

```
┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│  daemon A    │    │  daemon B    │    │  daemon C    │
│  (host 1)    │    │  (host 2)    │    │  (host 3)    │
└──────┬───────┘    └──────┬───────┘    └──────┬───────┘
       │                   │                   │
       ▼                   ▼                   ▼
  ┌─────────────────────────────────────────────────┐
  │            NATS cluster (TLS-mTLS)              │
  └─────────────────────────────────────────────────┘
       ▲                   ▲                   ▲
       │                   │                   │
┌──────┴───────┐    ┌──────┴───────┐    ┌──────┴───────┐
│  whatsapp    │    │  marketing   │    │  telegram    │
│  subprocess  │    │  subprocess  │    │  subprocess  │
│  (host 1)    │    │  (host 2)    │    │  (host 3)    │
└──────────────┘    └──────────────┘    └──────────────┘
```

Configuration:

```yaml
# broker.yaml
broker:
  type: nats
  url: "nats://nats.example.com:4222"
  auth:
    enabled: true
    nkey_file: /etc/nexo/nats.nkey
```

Daemon stamps subprocess plugins with `NEXO_BROKER_KIND=nats` +
`NEXO_BROKER_URL=<url>`, so each plugin connects to the same NATS
server independently. Cross-host fanout is NATS's job.

## Shape 2 — Server single-host (stdio bridge)

Daemon and plugins share one host. No NATS server installed; the
daemon's in-process `Local` broker (`tokio::mpsc`) handles every
event. Subprocess plugins reach the local broker through a JSON-RPC
stdio bridge piggybacking on the channel the daemon already opens
for `tool.invoke`.

```
┌─────────────────────────────────────────────────────────────────┐
│  daemon process (single host)                                   │
│                                                                 │
│  ┌─────────────────────────────────┐                           │
│  │  LocalBroker (tokio::mpsc)      │                           │
│  └──┬───────────────────────────┬──┘                           │
│     │                           │                              │
│     │ broker.publish            │ broker.subscribe forwarder   │
│     │ broker.event              │   pre-subscribed from        │
│     │                           │   manifest at boot           │
│     ▼                           ▼                              │
│  ┌─────────────────────────────────┐                           │
│  │  subprocess.rs JSON-RPC dispatch │                           │
│  │   - tool.invoke (existing)       │                           │
│  │   - broker.publish (81.14.b)     │                           │
│  │   - broker.event (81.14.b)       │                           │
│  └──┬─────────────────────────────┬─┘                           │
└─────┼─────────────────────────────┼─────────────────────────────┘
      │ stdin ◀──┐         ┌──▶ stdout
      ▼          │         │
┌─────────────┐  │         │  ┌─────────────┐
│  whatsapp   │──┤         ├──│  marketing  │
│  subprocess │  │         │  │  subprocess │
│             │  │         │  │             │
│  Stdio-     │  │         │  │  SDK's      │
│  Bridge-    │  │         │  │  Broker-    │
│  Broker     │  │         │  │  Sender +   │
│  (Phase 92) │  │         │  │  on_broker_ │
│             │  │         │  │  event      │
└─────────────┘  │         │  └─────────────┘
                 │         │
                 ▼         ▼
            (each plugin's StdioBridgeBroker holds an
             mpsc::Sender<Value> the SDK's PluginAdapter
             drains onto its single async stdout writer)
```

Configuration:

```yaml
# broker.yaml
broker:
  type: local
  url: ""
```

Daemon stamps subprocess plugins with `NEXO_BROKER_KIND=stdio_bridge`
and **omits** `NEXO_BROKER_URL` (no network endpoint). Each plugin's
`main.rs` reads the env, calls
`PluginAdapter::with_stdio_bridge_broker()`, and wraps the returned
`StdioBridgeBroker` in `AnyBroker::stdio_bridge`. From there the
plugin's existing `BrokerHandle::publish` / `.subscribe` code
keeps working unchanged.

Marketing uses the SDK's `BrokerSender` + `on_broker_event` hooks
directly (it never went through `AnyBroker::from_config`); those
hooks already route through the same stdout writer, so marketing
needs zero migration to participate in this shape.

## Shape 3 — Embedded (Android / iOS / WASM)

Daemon and plugins linked into a single binary as Rust crates.
No subprocess spawns at all; the `LocalBroker` works directly
because every plugin shares the same process memory.

```
┌─────────────────── APK / Bundle / WASM module ──────────────────┐
│                                                                 │
│  Single process:                                                │
│                                                                 │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │ daemon core  │  │ WhatsappPlugin│  │ MarketingPlugin│        │
│  │              │  │   (crate)    │  │   (crate)    │          │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘          │
│         │                  │                  │                  │
│         └──────────────────┴──────────────────┘                 │
│                            │                                     │
│                            ▼                                     │
│                ┌──────────────────────┐                          │
│                │  LocalBroker         │                          │
│                │  (tokio::mpsc        │                          │
│                │   in-process)        │                          │
│                └──────────────────────┘                          │
└─────────────────────────────────────────────────────────────────┘
```

Configuration:

```yaml
# broker.yaml
broker:
  type: local
```

No env vars stamped — there's no subprocess. The host (Android JNI,
Flutter FFI shim, WASM glue) injects an `Arc<AnyBroker::Local>`
directly into each plugin factory. Use of stdio-bridge is impossible
in this shape because there's nothing on the other end of stdin.

Plugin crates already expose the surface for this; the build flip
is a feature flag on the daemon and a different `main` entrypoint
in the host shim. See Phase 90 (Android embed) for the concrete
build pipeline.

## Daemon env vars stamped on each subprocess plugin

| Env var | When stamped | Plugin reads when |
|---------|--------------|-------------------|
| `NEXO_BROKER_KIND` | always for `whatsapp` + `telegram` instance factories (Phase 92.4) | constructing the broker in `main.rs` |
| `NEXO_BROKER_URL` | only when KIND is `nats` (Phase 92.4 omits for stdio_bridge) | constructing the NATS BrokerInner |

For non-instance plugins (marketing today, plugins discovered via
`plugins.discovery.search_paths` without a per-instance factory)
the env clear path isn't applied; they inherit `NEXO_BROKER_KIND`
from the daemon's own env if the operator exported it before
launching the daemon. The Phase 92 dev-daemon.sh template seeds
`NEXO_BROKER_KIND` in the daemon's environment for this reason.

## Migration path from a pre-92 deployment

A single-host operator that installed NATS only because subprocess
plugins forced it (the pre-92 default behaviour) follows this
sequence after pulling a 92-or-later release:

1. Upgrade the daemon binary to one that contains Phase 92.
2. Upgrade each subprocess plugin binary (whatsapp, telegram) to
   one that contains the matching 92.6 migration.
3. Stop NATS: `sudo systemctl stop nats-server`.
4. Switch broker.yaml back to `type: local`.
5. Restart the daemon. Verify
   `plugins.discovery: plugin registry wire complete loaded=N
   invalid=… init_failed_total=0` in the log.
6. Exercise an end-to-end flow (e.g. send a WhatsApp message,
   expect the bot reply). The whole pipeline now runs without
   any external broker.

Cluster operators (Shape 1) are unaffected — `type: nats` continues
to work identically.

## Source map

| Concern | File |
|---------|------|
| `BrokerKind::StdioBridge` enum | `crates/config/src/types/broker.rs` |
| `StdioBridgeBroker` impl | `crates/broker/src/stdio_bridge.rs` |
| `AnyBroker::StdioBridge` variant | `crates/broker/src/any.rs` |
| Daemon-side `broker.publish` handler | `crates/core/src/agent/nexo_plugin_registry/subprocess.rs` (Phase 81.14.b) |
| Daemon-side `broker.event` forwarder | same file (auto-subscribe from manifest) |
| `seed_*_subprocess_env_for` helpers | `proyecto/src/main.rs` |
| SDK `with_stdio_bridge_broker` helper | `crates/microapp-sdk/src/plugin.rs` |
| Plugin migrations (consumers) | `nexo-rs-plugin-whatsapp/src/main.rs`, `nexo-rs-plugin-telegram/src/main.rs` |

Phase 92 (the stdio-bridge broker) shipped in v0.1.6 — see the
[release notes](https://github.com/lordmacu/nexo-rs/releases/tag/nexo-rs-v0.1.6).
Remaining sub-phases (an end-to-end integration test, Prometheus
metrics for the bridge) are tracked as follow-ups.
