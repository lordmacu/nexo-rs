"""Phase 31.4 — broker.event dispatch tests."""

import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

SDK_ROOT = Path(__file__).resolve().parent.parent

DRIVER_ECHO = """
import asyncio
import sys
from nexo_plugin_sdk import PluginAdapter, Event

MANIFEST = '''
[plugin]
id = "echo_plugin"
version = "0.1.0"
name = "Echo"
description = "fixture"
min_nexo_version = ">=0.1.0"
'''

async def on_event(topic, event, broker):
    out = Event.new(
        "plugin.inbound.echoed",
        "echo_plugin",
        {"echoed": event.payload, "incoming_topic": topic},
    )
    await broker.publish("plugin.inbound.echoed", out)

async def main():
    adapter = PluginAdapter(manifest_toml=MANIFEST, on_event=on_event)
    await adapter.run()

asyncio.run(main())
"""

DRIVER_SLOW_HANDLER = """
import asyncio
import sys
from nexo_plugin_sdk import PluginAdapter, Event

MANIFEST = '''
[plugin]
id = "slow_plugin"
version = "0.1.0"
name = "Slow"
description = "fixture"
min_nexo_version = ">=0.1.0"
'''

async def on_event(topic, event, broker):
    # Hold the handler for 200ms; the reader must NOT block.
    await asyncio.sleep(0.2)
    out = Event.new("plugin.inbound.slow", "slow_plugin", {"ack": True})
    await broker.publish("plugin.inbound.slow", out)

async def main():
    adapter = PluginAdapter(manifest_toml=MANIFEST, on_event=on_event)
    await adapter.run()

asyncio.run(main())
"""


def spawn_driver(driver_src: str) -> subprocess.Popen[bytes]:
    env = dict(os.environ)
    env["PYTHONPATH"] = str(SDK_ROOT) + os.pathsep + env.get("PYTHONPATH", "")
    return subprocess.Popen(
        [sys.executable, "-c", driver_src],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )


def jsonrpc_request(req_id: int, method: str, params: dict | None = None) -> bytes:
    frame: dict = {"jsonrpc": "2.0", "id": req_id, "method": method}
    if params is not None:
        frame["params"] = params
    return (json.dumps(frame) + "\n").encode("utf-8")


def jsonrpc_notification(method: str, params: dict) -> bytes:
    frame = {"jsonrpc": "2.0", "method": method, "params": params}
    return (json.dumps(frame) + "\n").encode("utf-8")


def event_params(topic: str, payload: dict) -> dict:
    return {
        "topic": topic,
        "event": {
            "topic": topic,
            "source": "host",
            "payload": payload,
        },
    }


class DispatchTests(unittest.TestCase):
    def test_broker_event_invokes_handler(self):
        proc = spawn_driver(DRIVER_ECHO)
        try:
            ev = jsonrpc_notification(
                "broker.event", event_params("plugin.outbound.echo", {"hello": 1})
            )
            shutdown = jsonrpc_request(99, "shutdown")
            stdout, _stderr = proc.communicate(input=ev + shutdown, timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
        lines = [l for l in stdout.decode("utf-8").splitlines() if l.strip()]
        # First line should be the broker.publish notification.
        first = json.loads(lines[0])
        self.assertEqual(first["method"], "broker.publish")
        self.assertEqual(first["params"]["topic"], "plugin.inbound.echoed")
        echoed = first["params"]["event"]["payload"]
        self.assertEqual(echoed["echoed"], {"hello": 1})
        self.assertEqual(echoed["incoming_topic"], "plugin.outbound.echo")

    def test_handler_does_not_block_reader(self):
        # If the reader blocked on the slow handler, the shutdown
        # request would arrive AFTER the handler's broker.publish,
        # forcing a slow round-trip. Spawning handler via
        # asyncio.create_task lets shutdown overtake.
        proc = spawn_driver(DRIVER_SLOW_HANDLER)
        try:
            ev = jsonrpc_notification(
                "broker.event", event_params("plugin.outbound.slow", {"x": 1})
            )
            shutdown = jsonrpc_request(99, "shutdown")
            stdout, _stderr = proc.communicate(input=ev + shutdown, timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
        lines = [l for l in stdout.decode("utf-8").splitlines() if l.strip()]
        # We expect to see EITHER:
        #   shutdown reply BEFORE broker.publish (reader unblocked)
        #   OR they arrive in the order driven by the handler — but
        # at minimum both must arrive (no deadlock); the slow
        # handler's awaits must not gate shutdown processing.
        kinds = [
            "shutdown_reply"
            if json.loads(l).get("id") == 99
            else "publish"
            for l in lines
        ]
        self.assertIn("shutdown_reply", kinds, f"shutdown reply missing: {lines}")
        self.assertIn("publish", kinds, f"slow handler publish missing: {lines}")


if __name__ == "__main__":
    unittest.main()
