# Python Template Extension

Stdlib-only Python extension for agent-rs. Zero external dependencies.

## Protocol

Line-delimited JSON-RPC 2.0 over stdin/stdout. The agent spawns this process,
sends `initialize`, then `tools/call`, finally `shutdown`.

## Sample tools

- `ping` — zero-arg smoke test, returns `{pong: true, received_at_unix}`
- `add` — takes `{a: number, b: number}`, returns `{sum}`

## Copy & customize

```bash
cp -r extensions/template-python extensions/my-tool
cd extensions/my-tool
chmod +x main.py                    # preserve execute bit if copied across filesystems
```

Edit `plugin.toml`:

```toml
[plugin]
id = "my-tool"                      # must match ^[a-z][a-z0-9_-]*$
version = "0.1.0"
name = "My Tool"

[capabilities]
# Tool names must match ^[a-z][a-z0-9_]*$ — underscores yes, hyphens
# no. Keep them in snake_case (aligned with Python/Rust identifiers);
# `get-weather` is rejected at manifest parse.
tools = ["my_func"]

[transport]
kind = "stdio"
command = "./main.py"
```

Edit `main.py` — add your tool to `TOOL_SCHEMAS` and `TOOLS`.

## Install

Restart the agent. Discovery scans `extensions/` on boot and the agent logs:

```
discovered extension id=my-tool transport=stdio
extension runtime ready ext=my-tool tools=1
extension tool registered agent=<your-agent> ext=my-tool tool=ext_my-tool_my_func
```

## Invoke

The LLM sees your tool as `ext_<plugin_id>_<tool_name>`. Example prompt:

> "call ext_my-tool_my_func with {…}"

## Notes

- Python 3.6+ required on host (or inside the Docker container — our image
  includes `python3-minimal`).
- `shutdown` is best-effort: the host may continue teardown without waiting
  for your ACK (especially on NATS transports/timeouts). Do critical cleanup
  continuously or on signal handlers, not only after sending a shutdown reply.
- Keep the shebang `#!/usr/bin/env python3` on the first line and the `+x`
  bit on `main.py` so `command = "./main.py"` works directly. The repo's
  `.gitattributes` pins `main.py` to Unix LF endings so a CRLF checkout
  (Windows without `core.autocrlf=input`) doesn't break the shebang.
  On filesystems that strip the exec bit, run `chmod +x main.py` after
  clone.
- For debugging, write to stderr (`print(..., file=sys.stderr)`); the agent
  forwards stderr lines to its own tracing output.
- Error handling pattern the template demonstrates:
  - `raise InvalidArgs(...)` → host emits JSON-RPC `-32602 Invalid params`.
    Use this for bad input from the LLM so it can self-correct.
  - Any other exception → `-32603 Internal error` + full traceback on
    stderr for operator debugging.
  - Parse errors (malformed JSON from the host) → `-32700 Parse error`.
  - Unknown tool name → `-32601 Method not found`.
- Hook handler demo (see `main.py`):
  - `before_message` with `text` containing `__banned_token__` →
    returns `{abort: true, reason: "..."}`, the agent drops the
    message.
  - `before_message` with leading whitespace in `text` → returns
    `{abort: false, override: {text: <stripped>}}`, the agent sees
    the rewritten event. Use this pattern for normalisation /
    redaction policies.
- Combined tool name `ext_<id>_<name>` must be ≤64 chars (OpenAI/MiniMax limit).
