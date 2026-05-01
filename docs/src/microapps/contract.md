# Microapp contract (Phase 83.6)

This page is the language-agnostic specification for what makes a
program a **nexo microapp**. Every microapp — whether built with
the Rust SDK, hand-written in Python, or shipped as a Go binary —
implements the wire protocol below. If your code passes this
contract, the daemon will load it.

Companion pages:

- [Building microapps in Rust](./rust.md) — the Rust SDK shortcut
  that hides the wire details when you don't need them.
- [Admin RPC](./admin-rpc.md) — the operator surface for managing
  agents/credentials/pairing/transcripts from inside a microapp.

## Wire protocol overview

A microapp is a child process the daemon launches once at boot
and keeps alive across multiple agent turns. Communication is
**line-delimited JSON-RPC 2.0** over stdio:

- `stdin` (daemon → microapp): one JSON-RPC frame per line, UTF-8.
- `stdout` (microapp → daemon): same shape; mixed responses +
  notifications + outbound requests.
- `stderr`: free-form log lines forwarded to the daemon's
  `tracing` subscriber. Microapps SHOULD prefix log lines with
  `[INFO]`, `[WARN]`, `[ERROR]` so the daemon can map them.

Every JSON-RPC frame is exactly **one line** (no embedded
newlines in the JSON). The daemon's reader splits on `\n`. A
microapp MUST flush stdout after every frame.

### Framing rules

| Direction | Shape | Notes |
|---|---|---|
| Daemon → microapp request | `{"jsonrpc":"2.0","id":<int>,"method":...,"params":...}` | Numeric id (incrementing). |
| Microapp → daemon response | `{"jsonrpc":"2.0","id":<int>,"result":...}` or `{...,"error":{"code":...,"message":...}}` | id MUST echo the request's. |
| Microapp → daemon outbound request | `{"jsonrpc":"2.0","id":"app:<uuid>","method":...,"params":...}` | id MUST start with `"app:"` to disambiguate from daemon-initiated. |
| Daemon → microapp response to outbound | `{"jsonrpc":"2.0","id":"app:<uuid>","result":...}` | Echoes the microapp's id. |
| Either direction notification | `{"jsonrpc":"2.0","method":...,"params":...}` (no `id`) | Fire-and-forget; never gets a response. |

## Methods (daemon → microapp)

These are the methods the daemon will call on your microapp.
Implement them all. Methods not in this list are reserved for
future versions; respond with error code `-32601` (method not
found) for forward-compat.

### `initialize`

Called once per microapp lifetime, immediately after spawn.
Returns the microapp's tool catalogue + declared capabilities.

```json
{"method":"initialize","params":{
  "extension_id":"agent-creator",
  "state_dir":"/path/to/.nexo/extensions/agent-creator/state",
  "config":{"...microapp-specific config from extensions.yaml..."}
}}
```

Result:

```json
{
  "tools":[
    {"name":"agent_creator_create","description":"...","input_schema":{...}}
  ],
  "version":"0.1.0"
}
```

### `tools/list`

Re-queried on every binding refresh. Same return shape as
`initialize.tools`. Microapps SHOULD return identical bytes
across calls so the daemon's tool-cache prefix matcher stays
warm.

### `tools/call`

The core agent-loop entry point. Carries the effective
[`BindingContext`](#binding-context) (the agent / channel /
account triple) and the LLM's tool-call args.

```json
{"method":"tools/call","params":{
  "tool":"agent_creator_create",
  "args":{"name":"alice"},
  "binding_context":{...},
  "inbound":{...}
}}
```

Result `{"output":<JSON>}` (success) or
`{"error":"description"}` (microapp-side failure — distinct from
JSON-RPC `error` which signals a protocol-level fault).

### `agents/updated`

Notification (no `id`). Fired when the daemon's `agents.yaml`
hot-reload picked up a change that affects this microapp's
binding surface. Payload includes the new agent IDs visible to
this microapp.

### `hooks/<name>`

Called when the daemon dispatches a hook the microapp registered
during `initialize` (Phase 83.3). Reply with a
[`HookDecision`](#hook-decision).

### `shutdown`

Called once before the daemon SIGTERMs the process. Microapps
should flush state and reply with `{"ok":true}` within 5 s. The
daemon will SIGKILL after 10 s regardless.

## Methods (microapp → daemon)

Outbound calls — capability-gated. The operator's
`extensions.yaml` lists which capabilities this microapp may use.

### `nexo/dispatch`

Phase 82.3. Send an outbound message via a channel plugin (e.g.
WhatsApp). Requires `dispatch_outbound` capability.

```json
{"id":"app:<uuid>","method":"nexo/dispatch","params":{
  "to":"+573000000000",
  "channel":"whatsapp",
  "body":"Hello"
}}
```

### `nexo/admin/*`

Phase 82.10. Operator-surface admin RPC: agents CRUD, credentials,
pairing, LLM keys, channels. Each method is gated by a separate
capability (`agents_crud`, `credentials_crud`, `pairing_initiate`,
`llm_keys_crud`, `channels_crud`). See
[admin-rpc.md](./admin-rpc.md) for the full surface.

## Notifications (daemon → microapp)

Fire-and-forget messages the daemon pushes when an event lands.
Microapps subscribe by holding the matching capability.

| Method | Capability | Payload | Phase |
|---|---|---|---|
| `nexo/notify/transcript_appended` | `transcripts_subscribe` | `{session_id, role, body, ts_ms}` | 82.11 |
| `nexo/notify/pairing_status_changed` | `pairing_initiate` | `{channel, instance, status}` | 82.10 |
| `nexo/notify/token_rotated` | `credentials_crud` | `{old_hash, new}` | 82.12 |
| `nexo/notify/agent_event` | `transcripts_subscribe` | `{kind, agent_id, payload}` | 82.11 |

## Shapes

### Binding context

Phase 82.1. Every `tools/call` carries this triple so the
microapp knows which agent / channel / account fired the tool.

```json
{
  "binding_context":{
    "agent_id":"ana",
    "channel":"whatsapp",
    "account_id":"acme",
    "binding_id":"whatsapp:acme",
    "binding_index":0
  }
}
```

`account_id` is the multi-tenant key. Multi-tenant SaaS microapps
key their per-tenant SQLite tables on this field. See
[multi-tenant SaaS walkthrough](../extensions/multi-tenant-saas.md).

### Inbound message reference

Phase 82.5. Carries the original inbound message metadata (sender,
timestamp, kind) so a tool handler can correlate to the trigger.

```json
{
  "inbound":{
    "kind":"whatsapp_message",
    "from":"+573000000000",
    "ts_ms":1735689600000,
    "session_id":"..."
  }
}
```

### Extension config

Loaded from `extensions.yaml.entries.<id>.config` and threaded
through `initialize.params.config`. Opaque to the daemon —
microapps validate their own schema (Phase 83.17 will add
boot-time schema validation as opt-in).

### Hook decision

Phase 83.3. The microapp's vote on whether a hook should proceed.

```json
{"vote":"allow|deny|abstain","reason":"...","metadata":{...}}
```

`abstain` is the default — microapps that don't know about a
particular hook should abstain rather than vote.

### Tool call request / response

Already shown above under `tools/call`. The `output` field on
success is opaque JSON; the LLM sees its stringified form.

### Error envelope

JSON-RPC `error` field follows the standard:

```json
{"code":-32000,"message":"...","data":{"...optional structured info..."}}
```

The range `-32000` to `-32099` is **reserved for nexo**. Codes
below `-32099` and standard JSON-RPC codes (`-32700` parse error,
`-32600` invalid request, `-32601` method not found, `-32602`
invalid params, `-32603` internal error) keep their RFC meaning.

## Conventions

### Tool name namespacing

Tools MUST be prefixed with the extension id followed by an
underscore: `<extension_id>_<tool>`. Examples:

- ✅ `agent_creator_create`
- ✅ `acme_billing_charge`
- ❌ `create` (unprefixed)
- ❌ `agent-creator/create` (wrong separator)

The daemon validates the prefix on every `initialize` /
`tools/list` and rejects unprefixed tools so the LLM never sees
two microapps' `send` tools competing.

### Reserved JSON-RPC error codes

`-32000` to `-32099` are reserved. Common codes microapps SHOULD
emit:

| Code | Meaning |
|---|---|
| `-32000` | Capability not granted |
| `-32001` | Tool input failed schema validation |
| `-32002` | Backend service unavailable |
| `-32003` | Rate limit (the microapp's own per-tool limit) |
| `-32004` | Auth error talking to the microapp's external service |
| `-32099` | Microapp internal error (catchall) |

### Timeouts

The daemon's default per-call timeout is 30 seconds.
`extensions.yaml.entries.<id>.timeout_secs` overrides per
microapp. A timeout closes the in-flight call but leaves the
process alive; the daemon will retry the next call normally.

## Backward compatibility

The contract evolves under these rules:

1. **Additive fields always**. New fields on existing shapes
   appear behind `#[serde(default)]` (Rust) / "missing key is
   default" (other langs). Microapps MUST NOT reject unknown
   fields.
2. **Deprecation requires N + N+1**. To remove a method or
   field, the daemon emits a `tracing::warn!` + admin-ui
   notice in release N. The actual removal lands in N+1.
3. **Capability matrix grows monotonically**. New capabilities
   default to `false` for existing microapps; old capabilities
   never silently change semantics.
4. **Wire format MUST stay UTF-8 line-JSON**. A switch to
   length-prefixed framing or binary protocol would be a
   breaking change requiring an explicit major-version bump
   coordinated with all SDK languages.

## Worked example: Python hello-world

A volunteer should be able to ship a working microapp in Python
using only this doc and the standard library:

```python
#!/usr/bin/env python3
import json
import sys

def respond(req_id, result):
    sys.stdout.write(json.dumps({
        "jsonrpc": "2.0", "id": req_id, "result": result
    }) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    req = json.loads(line)
    rid = req["id"]
    method = req["method"]
    if method == "initialize":
        respond(rid, {
            "tools": [{
                "name": "hello_world_greet",
                "description": "Echo a greeting",
                "input_schema": {"type": "object", "properties": {
                    "name": {"type": "string"}
                }, "required": ["name"]}
            }],
            "version": "0.1.0"
        })
    elif method == "tools/call":
        name = req["params"]["args"]["name"]
        respond(rid, {"output": {"greeting": f"hello, {name}"}})
    elif method == "tools/list":
        respond(rid, {"tools": [...]})  # same as initialize
    elif method == "shutdown":
        respond(rid, {"ok": True})
        break
    else:
        sys.stdout.write(json.dumps({
            "jsonrpc": "2.0", "id": rid,
            "error": {"code": -32601, "message": f"unknown method: {method}"}
        }) + "\n")
        sys.stdout.flush()
```

Drop this in `extensions/hello/main.py`, mark executable, add
`extensions.yaml.entries.hello: { path: "extensions/hello/main.py" }`,
and `nexo ext install ./extensions/hello`. The daemon will load
it and the LLM will see `hello_world_greet` in its tool catalogue.

## Worked example: Go skeleton

Same protocol, idiomatic Go I/O:

```go
package main

import (
    "bufio"
    "encoding/json"
    "fmt"
    "os"
)

type RPC struct {
    JSONRPC string          `json:"jsonrpc"`
    ID      interface{}     `json:"id,omitempty"`
    Method  string          `json:"method,omitempty"`
    Params  json.RawMessage `json:"params,omitempty"`
    Result  interface{}     `json:"result,omitempty"`
    Error   *RPCError       `json:"error,omitempty"`
}

type RPCError struct {
    Code    int    `json:"code"`
    Message string `json:"message"`
}

func main() {
    scanner := bufio.NewScanner(os.Stdin)
    enc := json.NewEncoder(os.Stdout)
    for scanner.Scan() {
        var req RPC
        json.Unmarshal(scanner.Bytes(), &req)
        switch req.Method {
        case "initialize":
            enc.Encode(RPC{JSONRPC: "2.0", ID: req.ID, Result: map[string]interface{}{
                "tools":   []map[string]interface{}{{
                    "name":         "hello_go_greet",
                    "description":  "Echo a greeting",
                    "input_schema": map[string]interface{}{"type": "object"},
                }},
                "version": "0.1.0",
            }})
        // tools/call, tools/list, shutdown … same pattern
        default:
            enc.Encode(RPC{JSONRPC: "2.0", ID: req.ID, Error: &RPCError{
                Code: -32601, Message: fmt.Sprintf("unknown: %s", req.Method),
            }})
        }
    }
}
```

## Worked example: TypeScript / Node skeleton

```typescript
import * as readline from 'readline';

const rl = readline.createInterface({ input: process.stdin });

function respond(id: any, result: any) {
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n');
}

rl.on('line', (line) => {
  const req = JSON.parse(line);
  switch (req.method) {
    case 'initialize':
      respond(req.id, {
        tools: [{
          name: 'hello_ts_greet',
          description: 'Echo a greeting',
          input_schema: { type: 'object' }
        }],
        version: '0.1.0'
      });
      break;
    // tools/call, tools/list, shutdown — same pattern
    default:
      process.stdout.write(JSON.stringify({
        jsonrpc: '2.0', id: req.id,
        error: { code: -32601, message: `unknown: ${req.method}` }
      }) + '\n');
  }
});
```

## Reference: Rust SDK shortcut

For Rust microapps, the `nexo-microapp-sdk` crate (Phase 83.4)
hides the wire details. See [Building microapps in
Rust](./rust.md) for the high-level API. The SDK implements this
contract verbatim — anything you can do via the SDK you can do by
hand, but the SDK is the recommended path because it stays in
lockstep with the daemon's contract version.
