# Templates — language-by-language reference

This page lists the **starting points** for authoring a nexo
microapp in each supported language.

The **contract** ([contract.md](./contract.md)) is the source of
truth — line-delimited JSON-RPC over stdio. Every template
below ships a working `initialize → tools/list → tools/call →
shutdown` loop against that contract. They differ only in
ergonomics and per-language idioms.

## Rust (recommended) — `nexo-microapp-sdk`

**Where:** `extensions/template-microapp-rust/` in the
nexo-rs repo.

**Why use the SDK:** the daemon's contract version evolves under
N+N+1 deprecation rules. The Rust SDK lives in lockstep with
the daemon, so an additive field on the wire becomes an
additive field on `ToolCtx` / `HookCtx` automatically. Hand-
rolled parsers risk silent drift.

**Quick start:**

```bash
cp -r /path/to/nexo-rs/extensions/template-microapp-rust ./mi-microapp
cd ./mi-microapp
# rename in Cargo.toml + plugin.toml + src/main.rs
cargo build --release
```

See [rust.md](./rust.md) for the full SDK reference and
[getting-started.md](./getting-started.md) for the 1-hour
walkthrough.

**SDK feature flags:**

| Feature | Adds |
|---|---|
| (default) | `Microapp` builder + tool/hook handlers |
| `outbound` | `OutboundDispatcher` for `nexo/dispatch` outbound calls |
| `admin` | `AdminClient` for `nexo/admin/*` calls (capability-gated) |
| `test-harness` | `MicroappTestHarness` + `MockBindingContext` for unit tests |

## Python — hand-rolled (stdlib only)

No SDK ships today. Authors implement the wire protocol
directly using `sys.stdin` / `sys.stdout` / `json`. The
contract doc has a full worked example.

**Skeleton:**

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
                "name": "myapp_greet",
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

`plugin.toml`:

```toml
[plugin]
id = "my-python-microapp"
version = "0.1.0"
name = "My Python Microapp"

[capabilities]
tools = ["myapp_greet"]

[transport]
kind = "stdio"
command = "python3"
args    = ["./main.py"]
```

**Library tips:**

- `pydantic` for the JSON-RPC envelopes if you want typed
  parsing.
- `anyio` if you need async tool handlers.
- For test, run the binary as a subprocess and pipe JSON-RPC
  frames in/out.

## TypeScript / Node — hand-rolled

Same shape as Python; Node's `readline` does the line-splitting.

**Skeleton:**

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
          name: 'myapp_greet',
          description: 'Echo a greeting',
          input_schema: { type: 'object' }
        }],
        version: '0.1.0'
      });
      break;
    case 'tools/call':
      respond(req.id, { output: { greeting: `hello, ${req.params.args.name}` } });
      break;
    case 'shutdown':
      respond(req.id, { ok: true });
      process.exit(0);
    default:
      process.stdout.write(JSON.stringify({
        jsonrpc: '2.0', id: req.id,
        error: { code: -32601, message: `unknown: ${req.method}` }
      }) + '\n');
  }
});
```

`plugin.toml`:

```toml
[plugin]
id = "my-ts-microapp"

[transport]
kind = "stdio"
command = "node"
args    = ["./dist/main.js"]
```

**Library tips:**

- `@types/node` for stdio types.
- `zod` for tool input schema validation server-side.
- `bun` works as a drop-in for `node` and gives faster startup.

## Go — hand-rolled

Same shape; `bufio.Scanner` for line reading.

**Skeleton:**

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
                    "name":         "myapp_greet",
                    "description":  "Echo a greeting",
                    "input_schema": map[string]interface{}{"type": "object"},
                }},
                "version": "0.1.0",
            }})
        case "shutdown":
            enc.Encode(RPC{JSONRPC: "2.0", ID: req.ID, Result: map[string]bool{"ok": true}})
            return
        default:
            enc.Encode(RPC{JSONRPC: "2.0", ID: req.ID, Error: &RPCError{
                Code: -32601, Message: fmt.Sprintf("unknown: %s", req.Method),
            }})
        }
    }
}
```

`plugin.toml`:

```toml
[transport]
kind = "stdio"
command = "./my-go-microapp"   # the compiled binary
```

## Choosing a language

| Use case | Recommended stack |
|---|---|
| Multi-tenant SaaS, performance-sensitive | Rust + SDK |
| Quick prototype / glue to existing Python data pipeline | Python + stdlib |
| TypeScript shop, integration with web ecosystem | TypeScript + stdlib |
| Single-binary distribution to ops, no runtime dep | Go + stdlib |

**Rule of thumb:** if your microapp is the **product**, use
Rust + SDK so contract evolution is automatic. If your
microapp glues to another runtime you already maintain, use
the host language and pin the contract version explicitly in
your code.

## Contract version pinning

Whichever language you pick, your microapp MUST be aware of the
contract version it was tested against. The Rust SDK pins it
via `Cargo.toml = "0.1"`; hand-rolled microapps MUST embed a
constant + assert at boot.

```python
NEXO_CONTRACT_VERSION = "0.1"
# Future: read daemon's `initialize` response for a contract_version
# field and warn if it disagrees.
```

The contract doc's [backward compat](./contract.md#backward-compatibility)
rules apply: additive fields always, deprecation N + N+1, wire
format frozen.

## See also

- [contract.md](./contract.md) — language-agnostic spec
- [rust.md](./rust.md) — Rust SDK reference
- [getting-started.md](./getting-started.md) — 1-hour walkthrough
- [compliance-primitives.md](./compliance-primitives.md) — when
  to use which compliance helper (Rust today; spec is portable)
