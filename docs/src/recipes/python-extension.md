# Python extension

Ship a custom tool written in Python — no dependencies beyond
stdlib. The agent spawns your script, handshakes with it over
stdin/stdout, and exposes your tool to the LLM.

## Prerequisites

- `python3` on the host `$PATH`
- A running nexo-rs install with `extensions.enabled: true`

## 1. Copy the template

```bash
cp -r extensions/template-python extensions/word-count
cd extensions/word-count
```

## 2. Edit `plugin.toml`

```toml
[plugin]
id = "word-count"
version = "0.1.0"
description = "Count words in a piece of text."
priority = 0

[capabilities]
tools = ["count_words"]

[transport]
type = "stdio"
command = "python3"
args = ["./main.py"]

[requires]
bins = ["python3"]

[meta]
license = "MIT OR Apache-2.0"
```

`[requires] bins = ["python3"]` gates the extension: if Python
isn't on `$PATH`, the runtime skips the extension with a warn log
instead of crash-looping.

## 3. Write `main.py`

```python
#!/usr/bin/env python3
import sys, json

def reply(id, result=None, error=None):
    msg = {"jsonrpc": "2.0", "id": id}
    if error is None:
        msg["result"] = result
    else:
        msg["error"] = error
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

def log(*args):
    print(*args, file=sys.stderr, flush=True)

HANDSHAKE = {
    "server_version": "0.1.0",
    "tools": [{
        "name": "count_words",
        "description": "Count whitespace-separated words in a string.",
        "input_schema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        }
    }],
    "hooks": []
}

def main():
    log("word-count starting")
    for line in sys.stdin:
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = req.get("method", "")
        rid = req.get("id")
        if method == "initialize":
            reply(rid, HANDSHAKE)
        elif method == "tools/count_words":
            params = req.get("params", {}) or {}
            text = params.get("text", "")
            count = len(text.split())
            reply(rid, {"count": count})
        else:
            reply(rid, error={"code": -32601, "message": f"unknown method: {method}"})

if __name__ == "__main__":
    main()
```

Make it executable:

```bash
chmod +x main.py
```

## 4. Validate and install

```bash
cd ../..
./target/release/agent ext validate ./extensions/word-count/plugin.toml
./target/release/agent ext install ./extensions/word-count --link --enable
./target/release/agent ext doctor --runtime
```

`--link` creates a symlink instead of a copy — good for the
edit-test loop. `doctor --runtime` actually spawns the extension
and runs the handshake, so a Python error that kills the interpreter
during init surfaces here rather than in production logs.

## 5. Allow the tool per agent

The registered tool name is `ext_word-count_count_words`. Add it to
the right agent's `allowed_tools` (or use a glob):

```yaml
agents:
  - id: kate
    allowed_tools:
      - ext_word-count_*
      # ...
```

## 6. Run

```bash
./target/release/agent --config ./config
```

Send a message that would prompt the LLM to use the tool; watch
the logs for `tools/count_words` on stderr.

## Debugging

- **stderr of the Python process** is forwarded to the agent's log
  pipeline. `print(..., file=sys.stderr)` lines show up in the
  agent's tracing output with the `extension=word-count` field.
- **Handshake failures** are visible in `ext doctor --runtime` and
  prevent the tool from being registered at all.
- **Per-tool latency** shows up in the
  `nexo_tool_latency_ms{tool="ext_word-count_count_words"}`
  Prometheus histogram.

## Productionizing

- Pin `command` to an absolute path or a virtualenv-local
  interpreter; `python3` on `$PATH` may vary across hosts.
- Pick your dependency strategy carefully — the template is stdlib
  only. If you need `requests` or similar, ship a `requirements.txt`
  + bootstrap script, or switch to the Rust template.
- If the extension holds a connection to a remote service, add a
  heartbeat loop so you can detect liveness.
- For long-running tool calls, `print` status events to stderr —
  they become structured log entries and help debug hung tools.

## Cross-links

- [Extensions — templates](../extensions/templates.md)
- [Extensions — stdio](../extensions/stdio.md)
- [Skills — catalog](../skills/catalog.md)
