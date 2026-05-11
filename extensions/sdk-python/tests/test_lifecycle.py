"""Lifecycle tests for the Python plugin SDK (sub-phase 31.4.c).

Black-box tests: spawn a fresh `python3` child running a tiny
driver that imports `nexo_plugin_sdk.PluginAdapter`, feed it
JSON-RPC frames / signals, assert exit code + stderr markers.
"""

import json
import os
import select
import signal
import subprocess
import sys
import time
import unittest
from pathlib import Path

SDK_ROOT = Path(__file__).resolve().parent.parent

DRIVER_DOUBLE_RUN = """
import asyncio
import sys
from nexo_plugin_sdk import PluginAdapter, PluginError

MANIFEST = '''
[plugin]
id = "double_run_plugin"
version = "0.1.0"
name = "DoubleRun"
description = "fixture"
min_nexo_version = ">=0.1.0"
'''

async def main():
    adapter = PluginAdapter(manifest_toml=MANIFEST)
    await adapter.run()  # consumes the shutdown line, returns
    try:
        await adapter.run()
    except PluginError as e:
        sys.stderr.write(f"double-run-rejected: {e}\\n")
        sys.stderr.flush()
        return
    sys.stderr.write("ERROR: second run() did not raise\\n")
    sys.stderr.flush()
    sys.exit(3)

asyncio.run(main())
"""

DRIVER_IDLE = """
import asyncio
from nexo_plugin_sdk import PluginAdapter

MANIFEST = '''
[plugin]
id = "idle_plugin"
version = "0.1.0"
name = "Idle"
description = "fixture"
min_nexo_version = ">=0.1.0"
'''

async def main():
    adapter = PluginAdapter(manifest_toml=MANIFEST)
    await adapter.run()

asyncio.run(main())
"""

DRIVER_SLOW_DRAIN = """
import asyncio
import sys
from nexo_plugin_sdk import PluginAdapter, Event

MANIFEST = '''
[plugin]
id = "slow_drain_plugin"
version = "0.1.0"
name = "SlowDrain"
description = "fixture"
min_nexo_version = ">=0.1.0"
'''

async def on_event(topic, event, broker):
    sys.stderr.write("HANDLER_STARTED\\n")
    sys.stderr.flush()
    await asyncio.sleep(0.5)
    await broker.publish("plugin.inbound.done", Event.new("plugin.inbound.done", "slow_drain_plugin", {"ack": True}))
    sys.stderr.write("HANDLER_DONE\\n")
    sys.stderr.flush()

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


def shutdown_frame(req_id: int = 1) -> bytes:
    return (json.dumps({"jsonrpc": "2.0", "id": req_id, "method": "shutdown"}) + "\n").encode("utf-8")


def broker_event_frame(topic: str, payload: dict) -> bytes:
    frame = {
        "jsonrpc": "2.0",
        "method": "broker.event",
        "params": {"topic": topic, "event": {"topic": topic, "source": "host", "payload": payload}},
    }
    return (json.dumps(frame) + "\n").encode("utf-8")


def _read_until(stream, needle: bytes, timeout: float) -> bytes:
    """Drain ``stream`` (a pipe fd) until ``needle`` appears or timeout."""
    deadline = time.monotonic() + timeout
    seen = b""
    while needle not in seen and time.monotonic() < deadline:
        ready, _, _ = select.select([stream], [], [], 0.1)
        if ready:
            chunk = os.read(stream.fileno(), 4096)
            if not chunk:
                break
            seen += chunk
    return seen


class LifecycleTests(unittest.TestCase):
    def test_double_run_rejected(self):
        proc = spawn_driver(DRIVER_DOUBLE_RUN)
        try:
            stdout, stderr = proc.communicate(input=shutdown_frame(), timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
        self.assertEqual(proc.returncode, 0, f"non-zero exit; stderr={stderr!r}")
        self.assertIn(
            "double-run-rejected",
            stderr.decode("utf-8"),
            f"second run() must raise PluginError; stderr={stderr!r}",
        )

    def test_sigterm_exits_zero(self):
        # No work in flight, stdin left open: SIGTERM must convert to a
        # graceful exit (code 0), NOT the default terminate (-15).
        proc = spawn_driver(DRIVER_IDLE)
        try:
            time.sleep(0.4)  # let the child enter run() + register handlers
            proc.send_signal(signal.SIGTERM)
            stdout, stderr = proc.communicate(timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
        self.assertEqual(
            proc.returncode, 0, f"SIGTERM must exit 0, got {proc.returncode}; stderr={stderr!r}"
        )

    def test_sigterm_drains_inflight_handler(self):
        # A broker.event handler is mid-sleep when SIGTERM arrives; the
        # adapter must drain it (handler completes its publish) before
        # exiting 0.
        proc = spawn_driver(DRIVER_SLOW_DRAIN)
        try:
            proc.stdin.write(broker_event_frame("plugin.outbound.go", {"x": 1}))
            proc.stdin.flush()
            err_so_far = _read_until(proc.stderr, b"HANDLER_STARTED", timeout=8)
            self.assertIn(b"HANDLER_STARTED", err_so_far, f"handler never started; stderr={err_so_far!r}")
            proc.send_signal(signal.SIGTERM)
            rest_out, rest_err = proc.communicate(timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
        full_err = err_so_far + rest_err
        self.assertEqual(proc.returncode, 0, f"must exit 0; stderr={full_err!r}")
        self.assertIn(b"HANDLER_DONE", full_err, f"handler must be drained to completion; stderr={full_err!r}")
        lines = [l for l in rest_out.decode("utf-8").splitlines() if l.strip()]
        methods = [json.loads(l).get("method") for l in lines]
        self.assertIn("broker.publish", methods, f"drained handler's publish missing; stdout={lines}")


if __name__ == "__main__":
    unittest.main()
