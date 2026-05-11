# Python plugin SDK

Author plugins in Python that the daemon spawns as subprocesses,
talking the same JSON-RPC 2.0 wire format used by the Rust SDK in
[`crates/microapp-sdk/`](https://github.com/lordmacu/nexo-rs/tree/main/crates/microapp-sdk).
The robustness defaults (stdout guard, frame cap, signal handling)
match the [TypeScript](./typescript-sdk.md) and [PHP](./php-sdk.md)
SDKs (sub-phase 31.4.c).

Reference template:
[`extensions/template-plugin-python/`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-python)
(or run `nexo plugin new --lang python`). The SDK package lives in the
[`nexo-plugin-sdks`](https://github.com/lordmacu/nexo-plugin-sdks) repo
(`python/` subdir) and ships on PyPI as
[`nexoai`](https://pypi.org/project/nexoai/) — `pip install nexoai`
(the `nexo-plugin-sdk` name was taken; the importable module is still
`nexo_plugin_sdk`).

## Why subprocess + Python instead of an embedded interpreter

Running Python plugins as separate processes:

- Keeps the daemon language-agnostic; one wire contract, many
  SDK languages.
- Isolates plugin failures (a runaway Python plugin cannot
  panic the daemon).
- Sidesteps GIL coordination + PyO3 link-time complexity.

Daemon-side spawn code in
[`crates/core/src/agent/nexo_plugin_registry/subprocess.rs`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core/src/agent/nexo_plugin_registry)
treats the plugin as an opaque executable; Python plugins
re-use it without modification.

## Architecture summary

```
Operator host                         Plugin process
┌──────────────────┐    stdin   ┌──────────────────────────┐
│ daemon (Rust)    │──JSON-RPC──▶│ bin/<id> (bash launcher) │
│ subprocess host  │             │   exec python3 main.py   │
│                  │◀──JSON-RPC──│   PluginAdapter.run()    │
└──────────────────┘    stdout   └──────────────────────────┘
```

The bash launcher in `bin/<id>` sets `PYTHONPATH=lib/` and
exec's the vendored Python runtime so the plugin's deps come
from `lib/` only — no `site-packages` interference.

## Public API

```python
from nexo_plugin_sdk import (
    PluginAdapter,
    BrokerSender,
    Event, EventHandler, ShutdownHandler,
    PluginError, ManifestError, WireError,
    read_manifest,
    install_stdout_guard, uninstall_stdout_guard, is_stdout_guard_installed,
    STDOUT_GUARD_MARKER,
    MAX_FRAME_BYTES, JSONRPC_VERSION,
    serialize_frame, build_response, build_error_response, build_notification,
)
```

`PluginAdapter` constructor (all keyword-only):

| Parameter | Default | Description |
|-----------|---------|-------------|
| `manifest_toml: str` | *required* | Body of `nexo-plugin.toml`. Parsed + validated once at construction; the SDK checks `plugin.id` (incl. the `^[a-z][a-z0-9_]{0,31}$` slug regex the host enforces) and `plugin.version`. A failed construction leaves no stdout guard installed. |
| `server_version: str` | `"0.1.0"` | Returned in the `initialize` reply alongside the manifest. |
| `on_event` | `None` | `async (topic, Event, BrokerSender) -> None`. Invoked for every `broker.event` notification. Handler runs in a detached task; the dispatch loop continues reading stdin without blocking. |
| `on_shutdown` | `None` | `async () -> None`. Awaited before the SDK replies `{ok: true}` to the host's `shutdown` request. In-flight `on_event` (and `tool.invoke`) tasks are also awaited before returning. |
| `tools` | `None` | `list[ToolDef]` — the tool catalog advertised in the `initialize` reply's `tools` array (contract §4.1.1). Also settable post-construction via `.declare_tools([...])`. Every `name` must appear in the manifest's `[plugin.extends].tools` — a name that doesn't raises `ManifestError` at construction (mirrors the host's hard-failure). Omit → no `tools` array in the reply. |
| `on_tool` | `None` | `(ToolInvocation) -> Any`, sync or async. Dispatch handler for `tool.invoke` (contract §5.t). Runs on a detached task tracked by the shutdown drain. Mutually exclusive with `on_tool_with_context`. |
| `on_tool_with_context` | `None` | `(ToolInvocation, ToolContext) -> Any`, sync or async. Like `on_tool`, but `ctx.broker` is the same `BrokerSender` `on_event` gets — a tool body can `memory_recall` / `llm_complete` mid-invocation. Wins over `on_tool` when both are set. |
| `enable_stdout_guard: bool` | `True` | Replace `sys.stdout` with a line-buffering proxy that diverts non-JSON lines (a stray `print`) to stderr tagged `[stdout-guard]`. Blessed replies / `broker.publish` frames write through the captured original stdout, bypassing the guard. |
| `max_frame_bytes: int` | `MAX_FRAME_BYTES` (1 MiB) | Inbound JSON-RPC frames larger than this are rejected with a `WireError` log; dispatch continues. |
| `handle_process_signals: bool` | `True` | SIGTERM / SIGINT → graceful shutdown: drain in-flight handlers, then exit 0. `loop.add_signal_handler` is the primary path, falling back to `signal.signal` where unavailable (Windows ProactorEventLoop / non-main-thread). |

Calling `run()` twice raises `PluginError`. The stdin reader is fully
async (`loop.connect_read_pipe` + `asyncio.StreamReader`) — no
threadpool worker.

`Event` is a dataclass with `topic`, `source`, `payload`, optional
`correlation_id` + `metadata`. `BrokerSender.publish(topic, event)`
serializes a JSON-RPC notification to the captured original stdout
under an asyncio write lock.

### Stdout guard limitation

The guard only intercepts the text-stream API (`print`,
`sys.stdout.write`). A C extension or subprocess that writes to file
descriptor 1 directly bypasses it. Plugin authors who need stdout
output should use `print()` / `sys.stdout.write()`.

## Tool dispatch (`tool.invoke`, contract §4.1.1 + §5.t)

A plugin that declares `[plugin.extends].tools = ["myplugin_weather"]`
advertises a catalog of `ToolDef(name, description, input_schema)` and
handles one `tool.invoke` request per agent-loop tool call:

```python
from nexo_plugin_sdk import (
    PluginAdapter, ToolDef, ToolInvocation, ToolContext,
    ToolNotFound, ToolArgumentInvalid, ToolExecutionFailed, ToolUnavailable, ToolDenied,
    text_result,
)

async def on_tool(inv: ToolInvocation, ctx: ToolContext):
    if inv.tool_name != "myplugin_weather":
        raise ToolNotFound(inv.tool_name)
    city = (inv.args or {}).get("city")
    if not city:
        raise ToolArgumentInvalid("missing `city`", details={"field": "city"})
    # ctx.broker is the on_event broker handle — host calls work mid-invocation:
    # _ = await ctx.broker.memory_recall(agent_id=inv.agent_id or "", query=city)
    return text_result(f"Sunny in {city}")     # any JSON value is fine; this is the conventional shape

await PluginAdapter(
    manifest_toml=MANIFEST,
    tools=[ToolDef("myplugin_weather", "Current weather for a city",
                   {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]})],
    on_tool_with_context=on_tool,   # or on_tool=fn(inv) when you don't need the broker
).run()
```

The handler's return value becomes the JSON-RPC `result` verbatim
(non-JSON-serializable → `-33403`). Raising one of the `ToolInvocationError`
subclasses maps to the matching `-33401..-33405` code (with
`error.data.details` / `error.data.retry_after_ms` when set); an uncaught
generic exception maps to `-33403`; a `tool.invoke` with no handler
registered replies `-32601`. (PyPI `nexoai` ≥ 0.4.0.)

## Tarball convention (`noarch`)

Operators install Python plugins via the same
`nexo plugin install <owner>/<repo>[@<tag>]` CLI. The resolver
in `nexo-ext-installer` falls back to `noarch` when no
per-target tarball matches the daemon's host triple:

```
<id>-<version>-noarch.tar.gz
├── nexo-plugin.toml
├── bin/<id>           # bash launcher, mode 0755
└── lib/
    ├── plugin/main.py
    └── nexo_plugin_sdk/
        └── ...
```

The launcher (~5 LOC) reads:

```bash
#!/usr/bin/env bash
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec env PYTHONPATH="$DIR/lib" python3 "$DIR/lib/plugin/main.py" "$@"
```

## Pure-Python deps constraint

`noarch` requires that vendored deps work on every operator's
CPU. Native extensions (`*.so`, `*.pyd`, `*.dylib`) invalidate
the claim. The publish workflow's audit step runs
`scripts/verify-pure-python.sh` post-vendor and rejects any
tree containing those suffixes.

If your plugin needs a native dep, per-target Python tarballs
(`<id>-<version>-py312-x86_64-linux.tar.gz` etc.) are tracked
as Phase 31.4.b and not yet shipped.

## CI publish workflow

The shipped workflow in
[`extensions/template-plugin-python/.github/workflows/release.yml`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-python/.github/workflows/release.yml)
has the same 4-job shape as the Rust template (see
[Publishing a plugin](./publishing.md)) but:

- Build matrix has a single `noarch` entry.
- Build step uses `actions/setup-python@v5` + `pip install --target lib/`
  instead of `cargo zigbuild`.
- Vendor audit step calls `scripts/verify-pure-python.sh` to
  enforce the pure-Python constraint.

Sign + release jobs are identical to the Rust template; cosign
keyless OIDC ships `.sig` + `.pem` + `.bundle` per asset when
the `COSIGN_ENABLED` repo variable is `"true"`.

## Operator install flow (no changes for Python)

```bash
nexo plugin install your-handle/your-plugin@v0.2.0
```

Identical pipeline to the Rust install path:

1. Resolve release JSON.
2. Try `<id>-0.2.0-<host-triple>.tar.gz` (miss for noarch
   plugins).
3. **Fall back to `<id>-0.2.0-noarch.tar.gz`** (Phase 31.4
   addition).
4. Verify sha256.
5. Cosign verify per `trusted_keys.toml` (Phase 31.3).
6. Extract under `<dest_root>/<id>-0.2.0/`.
7. Daemon picks it up at next boot or hot-reload; spawns
   `bin/<id>` which exec's `python3 lib/plugin/main.py`.

## Local smoke test

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
    | python3 src/main.py
```

Should print one JSON-RPC response with your manifest +
`server_version`.

End-to-end test for the pack pipeline:

```bash
python3 -m unittest extensions/template-plugin-python/tests/test_pack_tarball.py -v
```

## SDK tests

In a clone of [`nexo-plugin-sdks`](https://github.com/lordmacu/nexo-plugin-sdks):

```bash
cd python
PYTHONPATH=. python3 -m unittest discover -v tests/
```

21 tests: handshake (incl. unknown-method `-32601`), manifest
validation (missing id, invalid TOML, id-regex violation), dispatch
(incl. non-blocking reader proof + oversized frame rejected with
continued dispatch), stdout guard (idempotent install, divert vs
passthrough, handler-print diverted while the blessed frame stays
clean), `broker.publish` back channel, lifecycle (double `run()`
rejected, SIGTERM exits 0, SIGTERM drains an in-flight handler).

## See also

- [Publishing a plugin (CI workflow)](./publishing.md) — Rust
  counterpart of the publish workflow this template is modeled
  after.
- [Plugin trust (`trusted_keys.toml`)](../ops/plugin-trust.md)
  — operator-side cosign verification policy that applies to
  Python plugins too.
- [Plugin contract](./contract.md) — wire format both SDKs
  implement.
