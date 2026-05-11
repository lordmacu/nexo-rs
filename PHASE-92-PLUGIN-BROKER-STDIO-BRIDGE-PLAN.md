# Phase 92 — Plugin broker stdio-bridge (cross-process local broker)

**Goal:** Wire a third broker transport — `StdioBridgeBroker` — that
pipes `broker.publish`, `broker.subscribe`, and `broker.event`
notifications over the existing JSON-RPC stdio channel between the
nexo daemon and each subprocess plugin. After Phase 81.18.b shipped
the subprocess flip for marketing / whatsapp / telegram, those
plugins running in a separate OS process can no longer reach the
daemon's in-process `Local` broker (`tokio::mpsc`, an in-memory
abstraction without a network endpoint). The stdio-bridge restores
that path without forcing operators to run a separate NATS server.

**Strategic context:** Three deployment shapes for the nexo daemon
become first-class instead of one:

| Shape | Broker config | Plugins | Required infra | Today |
|-------|---------------|---------|----------------|-------|
| Server multi-host | `kind: nats, url: nats://…` | subprocess | NATS cluster | ✅ works |
| Server single-host | `kind: local` | subprocess | none | ❌ silently breaks |
| Embedded (Android / iOS / WASM) | `kind: local` | lib-linked, no subprocess | none | ⚠ blocked by Phase 90 |

After this phase, shape #2 ("Server single-host") stops requiring
NATS. The deliverables shipped from `release.yml` (`.deb`, `.rpm`,
`.exe`, `.tar.xz` musl) become "just works" out of the box for the
single-host operator — drop the package, install plugins, the
daemon spawns each subprocess and bridges them through stdio.
Shape #3 (Android) is independent of this phase: lib-linking
plugins as Rust crates inside a single APK process means
`tokio::mpsc` works directly without crossing process boundaries,
no bridge needed. Phase 90 covers shape #3 on its own track.

The `Local` broker was originally documented in
[`proyecto/CLAUDE.md > Fault tolerance`](CLAUDE.md) as
*"NATS offline → fallback to local `tokio::mpsc` + disk queue,
drain on reconnect."* Today, every dev box silently abuses
`Local` as a steady-state broker (because in-tree plugins ran in
the same process). Post-81.18.b, that abuse silently breaks every
inbound topic. The stdio-bridge restores `Local` as a legitimate
steady-state mode for single-host deployments, matching its
documented role for the offline-fallback case and extending it to
the always-local case.

**Status:** ⬜ planned, **P2 — UNBLOCKS DISTRIBUTION**. Surfaced
2026-05-11 during whatsapp plugin install on
`agent-creator-microapp`. Workaround for the dev session: operator
installed `nats-server` via apt (Ubuntu universe) and switched
`broker.yaml type: nats` pointing at `127.0.0.1:4222`; the
plugin's eager start path then connected via NATS default URL and
inbound resumed. This phase is the proper fix that lets that
workaround be removed.

**Trigger:** any of —

- Distribution wave (`.deb` / `.rpm` / `.exe` / Termux tarball)
  needs to ship a working subprocess-plugin host without a
  separate NATS dependency the operator must install. Realistic
  if Phase 27.2 GA targets non-cluster single-box installs.
- A second operator surfaces "marketing/whatsapp deployed but
  silent" with broker connect errors in the daemon log (i.e.
  somebody else hits the same gotcha we hit on agent-creator).
- A dev surfaces "subprocess plugin broker connect refused" in
  a CI test that doesn't run NATS.

**Owner:** TBD — pull P0 tag once started.

## Mining

### OpenClaw — `research/`

| Path:line | Pattern |
|---|---|
| `research/src/plugins/plugin-host.ts:204-262` | OpenClaw's plugin host wraps a single ChildProcess per plugin and multiplexes a JSON-RPC channel over stdin/stdout. Methods include `tool.invoke`, `events.publish`, `events.subscribe`, `events.notify`. Confirms the multiplexed-stdio pattern is well-trodden — we're not inventing a new wire shape, just adding `broker.*` methods to the same pipe nexo already runs `tool.invoke` on. |
| `research/src/plugins/subprocess-loop.ts:38-95` | Subscriber registration on the host side: `subscriptionMap: Map<topic, Set<pluginId>>`, host listens on its own event bus, forwards each event to every subscribed plugin's stdin. Mirrors what we need in `subprocess.rs` for `broker.subscribe`. |
| `research/extensions/canvas/index.ts:412-450` | The Canvas extension (browser plugin precedent) uses `events.publish` to broadcast page-loaded / navigation events. Validates that subprocess plugins publishing back to the host's event bus is a real production pattern, not just a theoretical concern. |

### claude-code-leak

| Path:line | Pattern |
|---|---|
| n/a | Claude Code does not run subprocess plugins — its tools are all in-process TS modules. The leak is silent on cross-process broker bridging. No useful precedent. |

### Internal precedent

| Path:line | Pattern |
|---|---|
| `crates/core/src/agent/nexo_plugin_registry/subprocess.rs:1480-1520` | The existing `initialize` + `tool.invoke` JSON-RPC channel already does request/response multiplexing over stdio. Adding `broker.publish` is the same shape — read JSON line, dispatch by `method` field. The infrastructure is there; we extend the dispatch table. |
| `crates/core/src/agent/nexo_plugin_registry/init_loop.rs:200-238` | `register_remote_tool_handlers_after_init` shows the pattern for wiring a subprocess capability into the host's registries (tool registry today, broker tomorrow). Same hook point. |
| Phase 81.20.a / b / c | Daemon-mediated `memory.*` / `llm.*` / `tool.dispatch` RPCs shipped 2026-05-01. The broker pipe is the fourth member of that family — same architectural idea (subprocess accesses host resources via JSON-RPC over stdio), different resource. |

### What we don't take from references

OpenClaw's TypeScript plugin host serializes JSON line-by-line over
stdio without back-pressure — a chatty publisher could OOM the
host buffer. We pick up the multiplexing pattern but layer a
bounded `mpsc` per subscriber (parked in
[`81.20.d.followup-flow-control`](FOLLOWUPS.md) for v2; v1 ships
without back-pressure under the assumption that single-host dev
boxes don't have runaway publishers). Claude Code provides no
useful pattern at all for this concern.

## Architectural overview

```
┌─────────────── daemon process ───────────────┐
│                                              │
│  AnyBroker (Local: tokio::mpsc)              │
│   ▲                                          │
│   │  publish / subscribe / event             │
│   │                                          │
│  ┌────────────────────────────┐              │
│  │ subprocess.rs              │              │
│  │  ─ JSON-RPC over stdin/    │              │
│  │    stdout per child        │              │
│  │  ─ tool.invoke (existing)  │              │
│  │  ─ broker.publish (new)    │              │
│  │  ─ broker.subscribe (new)  │              │
│  │  ─ broker.event noti (new) │              │
│  └────────────────────────────┘              │
└─────┬──────────────────┬─────────────────────┘
      │ stdin            │ stdout
      ▼                  ▲
┌─────────────── subprocess plugin ────────────┐
│                                              │
│  PluginAdapter (nexo-microapp-sdk)           │
│   ▲                                          │
│   │  receives broker.event notifications     │
│   │  emits broker.publish / .subscribe RPCs  │
│   │                                          │
│  StdioBridgeBroker (impl AnyBroker)          │
│                                              │
│  WhatsappPlugin / MarketingPlugin / …        │
│   uses the StdioBridgeBroker via the         │
│   AnyBroker trait — zero plugin code change  │
│   beyond constructor selection.              │
└──────────────────────────────────────────────┘
```

## Wire shape

Three new JSON-RPC methods land on the existing per-subprocess
JSON-RPC channel. All payloads are line-delimited JSON.

```jsonc
// Subprocess → daemon: publish event to the host broker
{
  "jsonrpc": "2.0", "id": 142, "method": "broker.publish",
  "params": {
    "topic": "plugin.inbound.whatsapp.smoketest",
    "payload": { "kind": "TextMessageReceived", ... }
  }
}
// Daemon reply
{ "jsonrpc": "2.0", "id": 142, "result": { "ok": true } }
```

```jsonc
// Subprocess → daemon: subscribe to a topic pattern
{
  "jsonrpc": "2.0", "id": 143, "method": "broker.subscribe",
  "params": { "topic_pattern": "plugin.outbound.whatsapp.>" }
}
// Daemon reply
{
  "jsonrpc": "2.0", "id": 143,
  "result": { "subscriber_id": "sub-9c05a761" }
}
```

```jsonc
// Daemon → subprocess: notification for each matching event
{
  "jsonrpc": "2.0", "method": "broker.event",
  "params": {
    "subscriber_id": "sub-9c05a761",
    "topic": "plugin.outbound.whatsapp.smoketest",
    "payload": { "kind": "SendMessage", "to": "+57…", ... }
  }
}
// No reply expected (one-way notification per JSON-RPC 2.0 spec)
```

`subscriber_id` is daemon-assigned per subscribe call so the
subprocess can route concurrent `broker.event` notifications back
to the right `Subscriber` instance.

`broker.unsubscribe(subscriber_id)` is the symmetric cleanup path
fired when a plugin drops a `Subscriber`.

## Sub-phases

### 92.1 — `BrokerKind::StdioBridge` enum + nexo-config plumbing   ⬜

**Done when:**

- [ ] `nexo-config::types::broker::BrokerKind` gains a
  `StdioBridge` variant. No `url` field needed (the transport is
  the inherited stdin/stdout, not a network endpoint).
- [ ] Serde round-trip test: `"stdio_bridge"` ↔ `BrokerKind::
  StdioBridge`.
- [ ] `BrokerInner::default()` stays at `Local` (operator never
  picks `StdioBridge` directly — the daemon derives it for
  subprocess plugins).
- [ ] LOC: ~20 + 2 tests.

### 92.2 — `StdioBridgeBroker` impl in nexo-broker   ⬜

**Done when:**

- [ ] New `nexo-broker::stdio_bridge::StdioBridgeBroker` struct
  implementing the `AnyBroker` trait surface (`publish`,
  `subscribe`, `unsubscribe`).
- [ ] Holds `Arc<Mutex<BufWriter<Stdout>>>` for serialized writes
  to stdout + a `tokio::broadcast::Sender<BrokerEvent>` plus a
  request/response correlation table for the publish ACK path.
- [ ] `publish(topic, payload)` serializes a `broker.publish`
  JSON-RPC request to stdout, awaits the daemon's `result: { ok:
  true }` reply via the correlation table.
- [ ] `subscribe(topic_pattern)` serializes a `broker.subscribe`
  request, receives a `subscriber_id` in the reply, returns a
  `Subscriber` that filters the broadcast channel by id.
- [ ] `unsubscribe(subscriber_id)` serializes the symmetric call.
- [ ] Stdin reader task (separate tokio task spawned at
  construction) parses each line, dispatches `broker.event`
  notifications to the broadcast channel keyed by subscriber id,
  and resolves publish ACK futures via correlation id.
- [ ] `AnyBroker::from_config(&BrokerInner { kind: StdioBridge,
  .. })` returns a `StdioBridgeBroker` wired to
  `std::io::stdin()` + `std::io::stdout()`.
- [ ] LOC: ~350 + 8 unit tests (per-method round-trip, topic
  glob match, multi-subscriber fanout, ack timeout, malformed
  reply handling, unsubscribe drop, broadcast lag, lifetime
  cleanup).

### 92.3 — Daemon-side bridge in `subprocess.rs`   ⬜

**Done when:**

- [ ] The JSON-RPC dispatch loop in
  `crates/core/src/agent/nexo_plugin_registry/subprocess.rs`
  recognizes the three new methods (`broker.publish`,
  `broker.subscribe`, `broker.unsubscribe`).
- [ ] `broker.publish` handler: read payload, call
  `self.host_broker.publish(topic, payload).await`, reply with
  `result: { ok: true }` or surface the broker error verbatim.
- [ ] `broker.subscribe` handler: register the topic pattern in
  a per-subprocess subscriber map, attach a forwarder task that
  on every matching message from the host broker writes a
  `broker.event` notification to the child's stdin. The
  forwarder task respects the manifest's
  `[plugin.capabilities.broker].subscribe` allowlist —
  unauthorized subscriptions return JSON-RPC error -32601
  (`MethodNotAllowed`) at handshake.
- [ ] `broker.unsubscribe` handler: drop the forwarder task,
  remove the entry from the subscriber map, reply ok.
- [ ] Per-subprocess subscriber map cleaned up on child exit
  (existing `Drop` impl on `SubprocessNexoPlugin`).
- [ ] LOC: ~450 + 6 unit tests (publish round-trip, subscribe
  glob matching with mock broker, fanout to multiple
  subscribers, unauthorized topic rejection, unsubscribe
  cleanup, child-exit cleanup).

### 92.4 — Daemon-side env seeding   ⬜

**Done when:**

- [ ] `proyecto/src/main.rs::seed_whatsapp_subprocess_env_for`,
  `seed_telegram_subprocess_env_for`, and the marketing
  equivalent set `NEXO_BROKER_KIND` based on `cfg.broker.kind`:
  - `cfg.broker.kind == Nats` → seed `NEXO_BROKER_KIND=nats` +
    `NEXO_BROKER_URL=<url>` (existing behaviour).
  - `cfg.broker.kind == Local` → seed
    `NEXO_BROKER_KIND=stdio_bridge` (no URL).
  - `cfg.broker.kind == StdioBridge` → unreachable; treat as
    config error (operator should not have picked this).
- [ ] LOC: ~40 across 3 helpers + 6 unit tests (one per
  cfg.broker.kind × per plugin).

### 92.5 — SDK PluginAdapter stdin multiplexer   ⬜

**Done when:**

- [ ] `nexo-microapp-sdk::plugin::PluginAdapter` stdin reader
  multiplexes `tool.invoke` requests (existing) and
  `broker.event` notifications (new).
- [ ] New method `PluginAdapter::bridge_broker(&self) -> Arc<dyn
  AnyBroker>` returns a `StdioBridgeBroker` that hooks into the
  adapter's stdout writer + a broadcast channel populated by
  the multiplexer.
- [ ] Multiplexer recognizes notifications (`method` present,
  no `id`) as broker events and routes via `subscriber_id`;
  treats requests (`method` + `id`) as tool invocations and
  routes via the existing `on_tool` callback.
- [ ] Lifetime: when the adapter shuts down, the multiplexer
  task drains gracefully and the broadcast channel closes so
  every `Subscriber` returns `RecvError::Closed`.
- [ ] LOC: ~300 + 5 unit tests (stdin demux per kind,
  notification routing, lifetime cleanup, malformed line
  handling, concurrent tool+broker traffic).

### 92.6 — Consumer plugin migrations   ⬜

**Done when:**

- [ ] `nexo-rs-plugin-whatsapp/src/main.rs` replaces the
  hardcoded `BrokerInner` constructor block with a helper
  `AnyBroker::from_kind_env()` that:
  - reads `NEXO_BROKER_KIND` (default `"nats"` for backwards
    compat).
  - if `nats`, also reads `NEXO_BROKER_URL` (required).
  - if `stdio_bridge`, calls `adapter.bridge_broker()`.
  - returns `Arc<dyn AnyBroker>` either way.
- [ ] `nexo-rs-plugin-telegram/src/main.rs` — same change.
- [ ] `nexo-rs-extension-marketing/src/main.rs` — same change.
- [ ] Each plugin's release CI bumps the `Cargo.toml` version
  to advertise the new dep on `nexo-microapp-sdk` ≥ X.Y.Z (the
  version that ships 92.5).
- [ ] LOC: ~60 per plugin × 3 plugins + 3 standalone tests.

### 92.7 — Integration test in proyecto   ⬜

**Done when:**

- [ ] New file `proyecto/crates/core/tests/
  plugin_broker_stdio_bridge.rs`.
- [ ] Spawns a real daemon with `broker.yaml type: local` and
  two fixture subprocess plugins (existing
  `tests/fixtures/mock_subprocess_plugin.rs` extended to take a
  topic to publish or subscribe to via env var).
- [ ] Plugin A subscribes to topic `T`; plugin B publishes 100
  messages on `T`. Assert all 100 arrive at A within 5 seconds.
- [ ] NO NATS server running during the test (CI runner has no
  NATS dep).
- [ ] Second test: plugin C subscribes to a topic NOT in its
  `[plugin.capabilities.broker].subscribe` allowlist; assert
  the daemon rejects the subscribe with the
  capability-denied JSON-RPC error.
- [ ] LOC: ~250 + extension of the fixture.

### 92.8 — Metrics + observability   ⬜

**Done when:**

- [ ] Daemon emits `nexo_bridge_subscribers_active{plugin_id}`
  gauge on the existing `/metrics` Prometheus endpoint.
- [ ] Daemon emits `nexo_bridge_messages_forwarded_total{
  plugin_id, direction}` counter (direction = `inbound` for
  plugin→daemon publishes, `outbound` for daemon→plugin event
  notifications).
- [ ] Daemon emits `nexo_bridge_publish_errors_total{
  plugin_id, reason}` counter for ack timeouts +
  capability-denied + serialization errors.
- [ ] Tracing spans wrap each `broker.publish` / `.subscribe`
  / `.event` so jaeger / chrome trace tooling can
  visualise the flow.
- [ ] LOC: ~60 + 3 tests (metric emission per kind).

### 92.9 — Docs + migration story   ⬜

**Done when:**

- [ ] `proyecto/CLAUDE.md > Fault tolerance` section updated:
  clarify that `Local` is a legitimate steady-state broker
  mode for single-host deployments — not just an
  offline-fallback. Add a third row to the broker options
  table.
- [ ] New file `proyecto/docs/architecture/broker.md`: ASCII
  diagram of the three broker shapes, when to pick each, what
  the daemon does to seed subprocesses. Link from CLAUDE.md.
- [ ] `nexo-rs-plugin-whatsapp/README.md` Daemon wiring section
  drops the env var `NEXO_BROKER_URL` from the required list
  for `kind: local` deployments; adds a brief note that the
  stdio-bridge is auto-selected.
- [ ] `agent-creator-microapp/scripts/dev-daemon.sh` reverts
  the broker.yaml seed back to `type: local` (the operator-side
  workaround installed during the Phase 92 discovery session
  becomes obsolete). Update the comment to point at the
  shipped phase instead of "planned".
- [ ] Release notes entry for whichever crate version ships
  this phase.

## Test plan

| Surface | Coverage |
|---|---|
| Unit | 35+ tests across 92.1, 92.2, 92.3, 92.4, 92.5 (each sub-phase ships its own). |
| Integration | 92.7 — end-to-end daemon + 2 subprocess plugins + no NATS. |
| Regression | Existing `cargo nextest run --workspace` suite must stay green (Phase 81.18.b test suite uses subprocess plugins with NATS — this phase adds the `Local` path without removing NATS support). |
| Manual smoke | `dev-daemon.sh` with `broker: local`, whatsapp pair flow, voice note round-trip. The exact smoke we ran with NATS workaround during discovery should pass with `nats-server` stopped. |
| Cross-platform | Linux x86_64 (CI native) + Linux aarch64 (cross via zigbuild) + macOS x86_64/arm64 + Windows (followup `92.followup-windows-stdio` confirms; if Windows stdio buffering breaks, fall back to anonymous pipe). |

## Risk register

| Risk | Mitigation |
|---|---|
| **Stdio back-pressure** — a runaway publisher in one subprocess could exhaust the daemon's forwarder buffer and OOM the host. | v1 ships with unbounded forwarder buffers (`tokio::sync::mpsc::unbounded_channel`); v2 (`92.followup-flow-control`) adds bounded channels with drop-oldest semantics. Acceptable for single-host dev/server where operator controls plugin set. |
| **Windows stdio buffering** — Windows tends to line-buffer stdout pipes; large `broker.event` notification volume may stall. | Existing `tool.invoke` path already runs on Windows fine; if 92.7 integration test fails on Windows runner, follow up with explicit `O_BINARY` / `SetNamedPipeHandleState` flags. Track as `92.followup-windows-stdio`. |
| **Capability bypass** — an out-of-tree subprocess plugin could request `broker.subscribe` for a topic outside its manifest's `[plugin.capabilities.broker]` allowlist. | 92.3 hard-enforces allowlist check on every `subscribe` and `publish` call; tested in 92.7. |
| **Plugin/daemon version skew** — old plugin shipped with `NEXO_BROKER_KIND=nats` hardcoded won't pick up the new env. | 92.6 ensures plugin checks the env var with a sensible default (`nats` for backwards compat). Migration path: bump plugin version, operator updates plugin tarball/RPM. Documented in 92.9 release notes. |
| **Latency regression vs NATS** — stdio JSON-RPC may be slower than NATS for high-throughput topics. | Benchmark target (92.7 extension): bridge round-trip ≤ 500 µs p99 on Linux single-host (NATS loopback baseline ~150 µs p99). If we miss, profile + optimize serializer (consider msgpack as `92.followup-msgpack-transport`). |

## Acceptance criteria

The phase ships when all of the following are true:

1. Daemon respawn cycle with `broker.yaml type: local` (no NATS
   running) shows `loaded=N invalid=0 init_failed_total=0` in the
   `plugins.discovery: plugin registry wire complete` log line.
2. WhatsApp / marketing / telegram subprocess plugins log
   `subprocess plugin ready` and accept inbound via the broker
   bridge.
3. `agent-creator-microapp` voice-note round-trip works
   end-to-end with NATS stopped (`sudo systemctl stop
   nats-server`).
4. `cargo nextest run --workspace` stays green in the proyecto
   workspace.
5. The new integration test (`plugin_broker_stdio_bridge.rs`)
   passes on Linux x86_64 + aarch64 CI runners.
6. Daemon's `/metrics` endpoint exposes the three new counters
   defined in 92.8.
7. Docs in 92.9 are merged.

## Migration impact

**Zero breaking changes for existing deployments.** The
`BrokerKind::Nats` config path continues working unchanged — the
daemon still seeds `NEXO_BROKER_KIND=nats` + `NEXO_BROKER_URL` to
subprocesses. `BrokerKind::Local` deployments stop silently
breaking on subprocess plugins. The plugin binary's `BrokerInner`
constructor changes, but operators on a published plugin tarball
/ RPM only see the new behaviour after upgrading both daemon AND
plugin to versions that ship this phase.

Concrete rollout sequence for an existing single-host operator:

1. Upgrade daemon to a release that contains Phase 92.
2. Upgrade each plugin binary to a release that contains the 92.6
   migration.
3. Stop NATS (if it was installed only for the subprocess-plugin
   workaround).
4. Switch `broker.yaml` back to `type: local`.
5. Restart daemon. Verify the discovery log shows
   `init_failed_total=0`.

No data migration. No config schema break (the new `StdioBridge`
variant is derived, not operator-set). Plugin manifests
(`plugin.toml`) unchanged.

## Follow-ups (out of scope, tracked in `FOLLOWUPS.md > 92.*`)

- **`92.followup-flow-control`** — bound the per-subscriber
  forwarder channel and apply drop-oldest / drop-newest on
  overflow. Required for trust boundaries when plugin set is
  not operator-controlled (e.g. third-party plugin marketplace
  scenarios).
- **`92.followup-broker-auth`** — promote the manifest
  `[plugin.capabilities.broker]` allowlist into a runtime
  enforcement on every publish/subscribe call (today only
  validated at handshake time). Tied to the broader capability
  enforcement work (Phase 81.29 family).
- **`92.followup-windows-stdio`** — confirm the bridge works on
  Windows under high notification volume; if stdio line buffering
  causes stalls, fall back to anonymous pipes or named pipes via
  `tokio::net::windows::named_pipe`.
- **`92.followup-msgpack-transport`** — if 92.7 latency benchmark
  shows JSON serializer dominates the bridge round-trip, switch
  the wire format to msgpack-rpc. Drop-in for `serde_json` →
  `rmp-serde`; behind a `BrokerKind::StdioBridge` sub-config.
- **`92.followup-trace-export`** — propagate the trace context
  (W3C traceparent) through `broker.event` notifications so
  cross-process spans link in jaeger / chrome trace tooling.
- **`92.followup-publisher-fanout`** — when N subscribers in
  different subprocess plugins watch the same topic, the host
  serializes the payload N times. Hoist payload serialization
  to the publisher side once and reuse for all subscribers
  (single-allocation broadcast).

## Capability inventory

No new env-toggle gating dangerous behaviour. `NEXO_BROKER_KIND`
is a derived hint from the daemon's own config, not operator
input. No `crates/setup/src/capabilities.rs::INVENTORY` entry
needed.

## Estimated effort

| Swimlane | Sub-phases | Days focused |
|---|---|---|
| A — broker transport (lib-side) | 92.1, 92.2 | 3 |
| B — daemon-side bridge | 92.3, 92.4, 92.8 | 3 |
| C — SDK + plugin consumers | 92.5, 92.6 | 2 |
| D — integration + docs | 92.7, 92.9 | 1 |
| **Total** | 9 sub-phases | **~9 dev-days** |

Swimlanes A and C are 100% parallelizable. With 2 engineers,
wall-clock drops to ~5-6 days. Solo: ~9 days.
