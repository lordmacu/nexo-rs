# nexo-plugin-sdk (Python)

Phase 31.4 — child-side SDK for nexo subprocess plugins written
in Python. Mirrors the Rust counterpart in
[`crates/microapp-sdk/`](../../crates/microapp-sdk/) — same wire
format ([`nexo-plugin-contract.md`](../../nexo-plugin-contract.md)),
different language.

The reference plugin template lives at
[`extensions/template-plugin-python/`](../template-plugin-python/);
copy that directory to start a new plugin.

## Public API

```python
from nexo_plugin_sdk import (
    PluginAdapter,        # async dispatch loop
    BrokerSender,         # write-only handle to publish events back
    Event,                # dataclass mirror of the host's broker event
    PluginError,          # base exception
    ManifestError,        # raised when nexo-plugin.toml is malformed
    WireError,            # raised on malformed JSON-RPC frames
)
```

## Minimal example

```python
import asyncio
from nexo_plugin_sdk import PluginAdapter, Event

MANIFEST = open("nexo-plugin.toml").read()

async def on_event(topic: str, event: Event, broker) -> None:
    out = Event.new(
        "plugin.inbound.my_kind",
        "my_plugin",
        {"echoed": event.payload},
    )
    await broker.publish("plugin.inbound.my_kind", out)

async def main() -> None:
    adapter = PluginAdapter(manifest_toml=MANIFEST, on_event=on_event)
    await adapter.run()

if __name__ == "__main__":
    asyncio.run(main())
```

## What the daemon expects

| Method | Direction | Reply |
|--------|-----------|-------|
| `initialize` | host → child | `{ manifest, server_version }` automatically — the SDK reads + caches your manifest TOML at construction time. |
| `broker.event` (notification) | host → child | No JSON reply. Your `on_event` handler runs in a detached task so the dispatch loop continues reading stdin while the handler awaits broker round-trips. |
| `shutdown` | host → child | `{ ok: true }` after flushing in-flight handler tasks. The SDK awaits every outstanding task spawned for a `broker.event` notification before returning the reply, so handlers do not get cancelled mid-flight. |

Full spec: [`nexo-plugin-contract.md`](../../nexo-plugin-contract.md).

## Tests

```bash
cd extensions/sdk-python
PYTHONPATH=. python3 -m unittest discover -v tests/
```

6 tests covering the handshake, dispatch, broker.publish back-channel,
shutdown lifecycle, unknown-method handling, and manifest validation.

## Phase tracking

- 31.4 (shipped, this package) — child-side SDK + 6 tests.
- 31.4.b (deferred) — per-target Python tarballs (`pyXY-<triple>` targets) for plugins that need native extensions.
- PyPI publish deferred — once the API stabilizes after 31.5
  (TypeScript SDK) lands, this package ships to PyPI as
  `nexo-plugin-sdk`. Until then plugin authors vendor it via
  `pack-tarball-python.sh`.
