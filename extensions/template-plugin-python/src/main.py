"""Phase 31.4 — Python plugin template entrypoint.

Mirrors the structure of `extensions/template-plugin-rust/src/main.rs`:
parse the bundled manifest, build a PluginAdapter, register an
echo handler, drive the dispatch loop until the daemon sends
`shutdown`.

Replace `on_event` with your own channel logic. Topics on the
allowlist for this plugin (per the manifest's
`[[plugin.channels.register]]` entry) are
`plugin.outbound.template_echo_py[.<instance>]` for inbound
events from the daemon and
`plugin.inbound.template_echo_py[.<instance>]` for messages you
send back through `broker.publish(...)`.
"""

import asyncio
import sys
from pathlib import Path

# When run from a packed tarball, `bin/<id>` sets PYTHONPATH to
# `lib/`, where the vendored SDK lives.
from nexo_plugin_sdk import Event, PluginAdapter

# Manifest is read from disk so `cargo`-equivalent build steps
# don't need to reach into source. The bash launcher cd's into
# the plugin's installed dir before exec'ing python3, so the
# manifest is always at `../nexo-plugin.toml` relative to this
# file — except in dev (`python3 src/main.py`), where it's at
# `../nexo-plugin.toml` too.
MANIFEST_PATH = Path(__file__).resolve().parent.parent / "nexo-plugin.toml"


async def on_event(topic: str, event: Event, broker) -> None:
    """Echo every inbound event back as `plugin.inbound.<kind>`."""
    out_topic = topic.replace("plugin.outbound.", "plugin.inbound.", 1)
    out = Event.new(
        out_topic,
        event.source if event.source else "template_plugin_python",
        {"echoed": event.payload, "incoming_topic": topic},
    )
    try:
        await broker.publish(out_topic, out)
    except Exception as e:  # pragma: no cover - defensive
        print(f"plugin: publish failed: {e}", file=sys.stderr)


async def on_shutdown() -> None:
    print("template_plugin_python: shutdown requested", file=sys.stderr)


async def main() -> None:
    manifest_toml = MANIFEST_PATH.read_text(encoding="utf-8")
    adapter = PluginAdapter(
        manifest_toml=manifest_toml,
        server_version="0.1.0",
        on_event=on_event,
        on_shutdown=on_shutdown,
    )
    await adapter.run()


if __name__ == "__main__":
    asyncio.run(main())
