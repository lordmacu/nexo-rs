# nexo Plugin Contract

| Field | Value |
|-------|-------|
| `contract_version` | `1.0.0` |
| Status | Stable |
| Authoritative reference | This document |
| Reference implementations | Host: `crates/core/src/agent/nexo_plugin_registry/subprocess.rs`. Rust child: `crates/microapp-sdk/src/plugin.rs` (feature `plugin`). |

This contract describes how an out-of-tree plugin binary
communicates with the nexo daemon. A conforming plugin can be
written in **any language** — Rust, Python, TypeScript, Go, etc.
— as long as it implements the protocol defined here.

## 1. Transport

- Plugin runs as a child process of the daemon.
- Daemon writes to the child's `stdin`. Child writes to its
  `stdout`.
- `stderr` is closed by the daemon (currently `/dev/null` —
  Phase 81.23 will collect it into structured tracing).
- Each direction is a stream of **newline-delimited UTF-8 lines**.
- Each line is exactly one **JSON-RPC 2.0** message —
  request, response, or notification.
- Lines must not exceed the platform pipe buffer (typically
  4 KiB on Linux); fragmenting one JSON object across multiple
  lines is **not** supported.

## 2. Manifest

The plugin ships a `nexo-plugin.toml` file — schema defined by
the `nexo-plugin-manifest` crate. The fields relevant to this
contract are:

```toml
[plugin]
id = "slack"                       # ASCII slug, ^[a-z][a-z0-9_]{0,31}$
version = "0.2.0"                  # semver
name = "Slack Channel"
description = "..."
min_nexo_version = ">=0.1.0"

[plugin.requires]
nexo_capabilities = ["broker"]

# Phase 81.14 — subprocess entrypoint.
[plugin.entrypoint]
command = "/usr/local/bin/plugin-slack"  # absolute path or PATH binary
args = ["--mode", "stdio"]               # optional
env = { "RUST_LOG" = "info" }            # optional, MUST NOT begin with "NEXO_"

# Phase 81.8 — channel kinds the plugin exposes. Drives the
# broker subscribe / publish allowlist (see §6).
[[plugin.channels.register]]
kind = "slack"
adapter = "SlackChannelAdapter"
```

The host parses this manifest at boot and uses
`plugin.id` to verify the child's identity in the `initialize`
handshake (§4.1). It uses `plugin.entrypoint.command` to spawn
the child process. Any env key beginning with `NEXO_` is
**rejected at boot** — those names are reserved for the daemon's
own runtime configuration.

## 3. JSON-RPC envelope

All frames are valid JSON-RPC 2.0:

### Request
```json
{
  "jsonrpc": "2.0",
  "id": <integer or string>,
  "method": "<method-name>",
  "params": <object | null>
}
```

### Response (success)
```json
{
  "jsonrpc": "2.0",
  "id": <same as request>,
  "result": <object | null>
}
```

### Response (error)
```json
{
  "jsonrpc": "2.0",
  "id": <same as request, null if request was un-parseable>,
  "error": {
    "code": <integer>,
    "message": "<string>"
  }
}
```

### Notification
A **notification** is a request **without** an `id` field. The
peer must not reply.
```json
{
  "jsonrpc": "2.0",
  "method": "<method-name>",
  "params": <object | null>
}
```

The contract uses notifications for unidirectional broker
events — see §5.

## 4. Lifecycle

### 4.1 `initialize` (host → child request)

After spawning the child, the daemon writes one `initialize`
request and awaits the response. The child must respond before
`NEXO_PLUGIN_INIT_TIMEOUT_MS` (default `5000`) elapses or the
daemon kills it and surfaces `PluginInitError::Other`.

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": { "nexo_version": "0.1.5" }
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "manifest": { "plugin": { "id": "slack", "version": "0.2.0", ... } },
    "server_version": "slack-0.2.0"
  }
}
```

The child **must** echo a manifest whose `plugin.id` matches the
id the daemon expected (the id under which the plugin was
registered in the factory registry). Mismatch is a hard failure
— the daemon kills the child and refuses to load the plugin.
This defends against an out-of-tree binary impersonating a
different plugin.

`server_version` is a free-form string identifying the running
binary; the SDK defaults it to `<id>-<version>` from the
manifest.

### 4.2 `shutdown` (host → child request)

The daemon sends `shutdown` when it wants the plugin to exit
gracefully. The child should flush state, then reply.

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "shutdown",
  "params": { "reason": "host requested" }
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": { "ok": true }
}
```

Reply with an `error` object instead of `result` if shutdown
fails — the host surfaces `PluginShutdownError::Other` to the
operator.

After the reply, the daemon waits **1 second** for the process
to exit on its own. If the child is still alive, the daemon
sends `SIGKILL`. So: reply, then exit.

## 5. Broker bridge

The wire-level shape of the broker bridge is two **notifications**:

### 5.1 `broker.event` (host → child)

Whenever the daemon's broker delivers an event on a topic
matching one of the plugin's outbound subscriptions (derived
from `manifest.channels.register[].kind` — see §6), the daemon
sends:

```json
{
  "jsonrpc": "2.0",
  "method": "broker.event",
  "params": {
    "topic": "plugin.outbound.slack.team_a",
    "event": {
      "id": "01940000-0000-0000-0000-000000000001",
      "timestamp": "2026-05-01T00:00:00Z",
      "topic": "plugin.outbound.slack.team_a",
      "source": "agent.coordinator",
      "session_id": "01940000-0000-0000-0000-000000000099",
      "payload": { "text": "hello", ... }
    }
  }
}
```

The `event` field is a serialised `nexo_broker::Event`. The
plugin processes the event (e.g. forwards `payload.text` to
Slack's API) and **may** reply with a `broker.publish`
notification (§5.2) — but it is **not required** to reply.

### 5.2 `memory.recall` (child → host request) <Phase 81.20.a>

When the plugin needs to look up agent memory entries, it issues
a JSON-RPC request to the daemon. Unlike `broker.event` /
`broker.publish` which are notifications, this is a
**request-response** flow: the child sends with an `id` and
awaits the matching reply.

**Child → host request:**
```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "method": "memory.recall",
  "params": {
    "agent_id": "ventas_v1",
    "query": "user prefers concise answers",
    "limit": 5
  }
}
```

**Host → child reply (success):**
```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "result": {
    "entries": [
      {
        "id": "01940000-0000-0000-0000-000000000001",
        "agent_id": "ventas_v1",
        "content": "user prefers concise answers",
        "tags": ["preference"],
        "concept_tags": [],
        "created_at": "2026-04-30T18:22:31Z",
        "memory_type": null
      }
    ]
  }
}
```

**Host → child reply (error):**
- `-32601` method not found (only `memory.recall` wired in 81.20.a;
  `llm.complete` / `tool.dispatch` ship in 81.20.b/.c).
- `-32602` invalid params (missing `agent_id` / wrong type for
  `query`).
- `-32603` memory not configured (operator hasn't enabled
  long-term memory) OR memory backend returned an error.

`limit` defaults to 10, capped hard at 1000. The handler calls
`LongTermMemory::recall(agent_id, query, limit)` which already
expands the query with up to 3 derived concept tags so FTS5 hits
memories whose stored content diverges from the query surface.

### 5.3 `llm.complete` (child → host request) <Phase 81.20.b>

When the plugin needs an LLM completion, it issues a request and
awaits the response.

**Child → host request:**
```json
{
  "jsonrpc": "2.0",
  "id": 50,
  "method": "llm.complete",
  "params": {
    "provider": "minimax",
    "model": "minimax-m2.5",
    "messages": [
      {"role": "user", "content": "summarize this in one line: ..."}
    ],
    "max_tokens": 1024,
    "temperature": 0.7,
    "system_prompt": "You answer concisely."
  }
}
```

`messages[].role` is one of `system`, `user`, `assistant`, `tool`.
`max_tokens` defaults to 4096; `temperature` defaults to 0.7;
`system_prompt` is optional.

**Host → child reply (success):**
```json
{
  "jsonrpc": "2.0",
  "id": 50,
  "result": {
    "content": "Concise reply text.",
    "finish_reason": "stop",
    "usage": {
      "prompt_tokens": 25,
      "completion_tokens": 8
    }
  }
}
```

`finish_reason` is one of `stop`, `length`, `tool_use`,
`other:<reason>`.

**Host → child reply (errors):**
- `-32602` invalid params (missing `provider` / `model` /
  `messages`, malformed message, empty messages array).
- `-32603` LLM not configured (operator hasn't wired the
  registry to the subprocess pipeline) OR client build failed
  (provider name not registered, config invalid) OR `chat()`
  call returned an error.
- `-32601` provider returned tool calls instead of text — MVP
  surfaces this as `not_implemented`. The tool-call wire shape
  (which lets the child re-submit `tool_result` follow-ups)
  lands in a future contract bump.

Daemon-side caps `max_tokens` at u32::MAX. Streaming via
`llm.complete.delta` notifications is on the roadmap (81.20.b.b)
— today the host buffers the full response and replies once.

### 5.4 `broker.publish` (child → host)

When the plugin wants to push an event onto the broker (e.g.
delivering an inbound message from Slack), it writes:

```json
{
  "jsonrpc": "2.0",
  "method": "broker.publish",
  "params": {
    "topic": "plugin.inbound.slack.team_a",
    "event": {
      "id": "01940000-0000-0000-0000-000000000002",
      "timestamp": "2026-05-01T00:01:00Z",
      "topic": "plugin.inbound.slack.team_a",
      "source": "slack",
      "session_id": null,
      "payload": { "from": "U01ABC", "text": "hi", ... }
    }
  }
}
```

The host **validates** the topic against the allowlist (§6)
**before** forwarding to the broker. Topics outside the
allowlist are dropped with a `tracing::warn!` log and **never**
reach the broker.

## 6. Topic allowlist

The host derives subscribe + publish patterns from the
manifest's `[[plugin.channels.register]]` entries.

For each entry with `kind = K`:

| Direction | Patterns |
|-----------|----------|
| Outbound (daemon → child) | `plugin.outbound.K`, `plugin.outbound.K.>` |
| Inbound (child → daemon)  | `plugin.inbound.K`, `plugin.inbound.K.>` |

Wildcard semantics follow `nexo_broker::topic::topic_matches`:

- `*` matches **exactly one** path segment.
- `>` matches **one or more** trailing segments (must have ≥1).
- Plain segments match literally.

So `plugin.inbound.slack.>` matches `plugin.inbound.slack.team_a`
and `plugin.inbound.slack.team_a.thread_42` but **not**
`plugin.inbound.slack` (no trailing segments). That's why both
exact and wildcard patterns are in the allowlist for each kind.

A child publish to a topic that does not match any pattern
in the allowlist is dropped — this is the host's primary defense
against a plugin attempting to hijack core nexo topics like
`agent.route.*` or `command.*`.

## 7. Error codes

`-32xxx` is JSON-RPC reserved range; nexo extensions live in
`-31xxx` (none used yet) and `-32000..-32099` (implementation
defined).

| Code | Meaning |
|------|---------|
| `-32700` | Parse error — line is not valid JSON |
| `-32600` | Invalid request — well-formed JSON but not JSON-RPC 2.0 |
| `-32601` | Method not found |
| `-32602` | Invalid params |
| `-32603` | Internal error |
| `-32000` | nexo: shutdown handler returned an error |
| `-32001..-32099` | Reserved for future nexo error variants |

The host translates each of these into a structured
`PluginInitError` or `PluginShutdownError` variant for operator
diagnostics.

## 8. Backpressure

The host's stdin writer feeds the child via a **bounded mpsc
channel** of depth 64. When the channel is full (the child is
processing more slowly than the broker is delivering events to
it), new `broker.event` notifications are **dropped with a
warn-level log** rather than blocking the daemon's broker.

This matches the at-most-once delivery semantics the broker
itself promises — no plugin should be relying on every event
arriving. Plugins that need durable delivery should subscribe
to a NATS jetstream stream out-of-band, which is outside the
scope of this contract.

## 9. Examples

### 9.1 Rust

Using the `nexo-microapp-sdk` crate with the `plugin` feature
(Phase 81.15.a):

```rust
use nexo_microapp_sdk::plugin::{PluginAdapter, BrokerSender};
use nexo_broker::Event;

const MANIFEST: &str = include_str!("../nexo-plugin.toml");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    PluginAdapter::new(MANIFEST)?
        .on_broker_event(|topic: String, event: Event, broker: BrokerSender| async move {
            // Outbound: deliver to the external service.
            // (Pseudocode; replace with your channel client.)
            let payload = event.payload.clone();
            let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
            send_to_slack(text).await;

            // Inbound: relay any reply back via the broker.
            let reply = Event::new(
                "plugin.inbound.slack",
                "slack",
                serde_json::json!({"echo": text}),
            );
            let _ = broker.publish("plugin.inbound.slack", reply).await;
        })
        .on_shutdown(|| async { Ok(()) })
        .run_stdio()
        .await?;
    Ok(())
}

async fn send_to_slack(_text: &str) {}
```

### 9.2 Python (skeleton — Phase 31.4 will publish a real SDK)

```python
import json
import sys

MANIFEST = open("nexo-plugin.toml").read()

def main():
    for line in sys.stdin:
        frame = json.loads(line)
        if "id" in frame:
            handle_request(frame)
        else:
            handle_notification(frame)

def handle_request(frame):
    if frame["method"] == "initialize":
        reply = {
            "jsonrpc": "2.0",
            "id": frame["id"],
            "result": {
                "manifest": parse_manifest(MANIFEST),
                "server_version": "slack-0.2.0",
            },
        }
        sys.stdout.write(json.dumps(reply) + "\n")
        sys.stdout.flush()
    elif frame["method"] == "shutdown":
        sys.stdout.write(json.dumps({
            "jsonrpc": "2.0",
            "id": frame["id"],
            "result": {"ok": True},
        }) + "\n")
        sys.stdout.flush()
        sys.exit(0)

def handle_notification(frame):
    if frame["method"] == "broker.event":
        topic = frame["params"]["topic"]
        event = frame["params"]["event"]
        # ... deliver event.payload to the external service ...
        # Optionally publish back:
        publish("plugin.inbound.slack", {"echo": event["payload"]})

def publish(topic, payload):
    note = {
        "jsonrpc": "2.0",
        "method": "broker.publish",
        "params": {"topic": topic, "event": build_event(topic, payload)},
    }
    sys.stdout.write(json.dumps(note) + "\n")
    sys.stdout.flush()

main()
```

### 9.3 TypeScript / Node (skeleton — Phase 31.5)

```ts
import * as readline from "node:readline";
import * as fs from "node:fs";

const MANIFEST = fs.readFileSync("nexo-plugin.toml", "utf-8");
const rl = readline.createInterface({ input: process.stdin });

rl.on("line", (line) => {
  const frame = JSON.parse(line);
  if ("id" in frame) handleRequest(frame);
  else handleNotification(frame);
});

function handleRequest(frame: any) {
  if (frame.method === "initialize") {
    write({
      jsonrpc: "2.0",
      id: frame.id,
      result: { manifest: parseManifest(MANIFEST), server_version: "slack-0.2.0" },
    });
  } else if (frame.method === "shutdown") {
    write({ jsonrpc: "2.0", id: frame.id, result: { ok: true } });
    process.exit(0);
  }
}

function handleNotification(frame: any) {
  if (frame.method === "broker.event") {
    // ... deliver frame.params.event.payload ...
    publish("plugin.inbound.slack", { echo: frame.params.event.payload });
  }
}

function publish(topic: string, payload: unknown) {
  write({
    jsonrpc: "2.0",
    method: "broker.publish",
    params: { topic, event: buildEvent(topic, payload) },
  });
}

function write(frame: unknown) {
  process.stdout.write(JSON.stringify(frame) + "\n");
}
```

## 10. Versioning + compatibility

This contract uses **semver**. The current version is `1.0.0`.

| Change kind | Semver bump |
|-------------|-------------|
| Add a new optional manifest field | minor |
| Add a new optional method (host or child) | minor |
| Add a new optional notification | minor |
| Add a new error code in `-32000..-32099` | minor |
| Remove or rename a method / notification / field | **major** |
| Change the JSON shape of a method's params or result | **major** |
| Tighten validation (e.g. rejecting previously-allowed input) | **major** |

Plugins should declare the contract version they target via the
manifest's `min_nexo_version` field plus a future
`contract_version` field (Phase 81.16 follow-up). The host
rejects plugins targeting a major version it does not support.

## 11. Reference implementations

- **Host adapter**: `crates/core/src/agent/nexo_plugin_registry/subprocess.rs`
  (`SubprocessNexoPlugin`) — Phase 81.14 + 81.14.b.
- **Rust child SDK**: `crates/microapp-sdk/src/plugin.rs`
  (`PluginAdapter`, feature `plugin`) — Phase 81.15.a.
- **Python SDK**: deferred to Phase 31.4.
- **TypeScript / Node SDK**: deferred to Phase 31.5.
- **Go SDK**: not yet planned.

## 12. Out of contract scope

The following are part of the broader plugin platform but are
deliberately out of THIS document's scope:

- **`memory.recall` / `llm.complete` / `tool.dispatch`** RPC bridges
  (Phase 81.20) — let the child invoke daemon-mediated framework
  services.
- **Supervisor + respawn + resource limits** (Phase 81.21).
- **Sandbox** (network + filesystem allowlist via manifest, Phase 81.22).
- **Stdio → tracing bridge** (Phase 81.23).
- **Plugin marketplace + signing** (Phase 31).

Each of these will either extend this contract additively (in
which case `contract_version` bumps minor) or live in a separate
contract document.

## 13. Changelog

| Version | Date | Changes |
|---------|------|---------|
| `1.0.0` | 2026-05-01 | Initial publication. Lifecycle (`initialize` / `shutdown`) + broker bridge (`broker.event` / `broker.publish`) + manifest `[plugin.entrypoint]` section. Host adapter shipped in Phase 81.14 + 81.14.b; Rust child SDK in Phase 81.15.a. |
| `1.1.0` | 2026-05-01 | Phase 81.20.a — `memory.recall` request-response added. Additive; existing 1.0.0 plugins continue to work unchanged. Manifest `[plugin.supervisor]` section (Phase 81.21.b) — additive. Host-side activation: Phase 81.17.b boot wire. Phase 81.21 supervisor + 81.21.b stderr tail capture. |
| `1.2.0` | 2026-05-01 | Phase 81.20.b — `llm.complete` request-response added. Additive. MVP supports text responses only; tool-call responses surface as `-32601 not_implemented`. Streaming (`llm.complete.delta` notifications) on roadmap as 81.20.b.b. Host-side runtime threading deferred to 81.20.b.b — daemon today returns `-32603 "llm not configured"` until main.rs threads `LlmServices` into the subprocess pipeline. |
