"""Phase 31.4 — handshake tests for the Python plugin SDK.

Each test spawns a fresh `python3` child running a tiny driver
script that imports `nexo_plugin_sdk.PluginAdapter`. We feed
JSON-RPC frames through stdin and assert the lines that come
back on stdout. The wire format is the source of truth so we
black-box test rather than touching internals.
"""

import asyncio
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

SDK_ROOT = Path(__file__).resolve().parent.parent

DRIVER_HANDSHAKE = """
import asyncio
import sys
from nexo_plugin_sdk import PluginAdapter

MANIFEST = '''
[plugin]
id = "test_plugin"
version = "0.1.0"
name = "Test"
description = "fixture"
min_nexo_version = ">=0.1.0"
'''

async def main():
    adapter = PluginAdapter(manifest_toml=MANIFEST, server_version="0.0.99")
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


class HandshakeTests(unittest.TestCase):
    def test_initialize_returns_manifest(self):
        proc = spawn_driver(DRIVER_HANDSHAKE)
        try:
            init = jsonrpc_request(1, "initialize")
            shutdown = jsonrpc_request(2, "shutdown")
            stdout, _stderr = proc.communicate(input=init + shutdown, timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
        lines = [l for l in stdout.decode("utf-8").splitlines() if l.strip()]
        self.assertGreaterEqual(len(lines), 2, f"expected ≥2 reply lines, got {lines}")
        first = json.loads(lines[0])
        self.assertEqual(first["jsonrpc"], "2.0")
        self.assertEqual(first["id"], 1)
        self.assertEqual(first["result"]["server_version"], "0.0.99")
        self.assertEqual(first["result"]["manifest"]["plugin"]["id"], "test_plugin")
        # Second line is the shutdown reply.
        second = json.loads(lines[1])
        self.assertEqual(second["id"], 2)
        self.assertTrue(second["result"]["ok"])

    def test_unknown_method_returns_error(self):
        proc = spawn_driver(DRIVER_HANDSHAKE)
        try:
            req = jsonrpc_request(7, "garbage")
            shutdown = jsonrpc_request(8, "shutdown")
            stdout, _stderr = proc.communicate(input=req + shutdown, timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
        lines = [l for l in stdout.decode("utf-8").splitlines() if l.strip()]
        first = json.loads(lines[0])
        self.assertEqual(first["id"], 7)
        self.assertIn("error", first)
        self.assertEqual(first["error"]["code"], -32601)

    def test_manifest_missing_id_raises(self):
        from nexo_plugin_sdk import ManifestError, PluginAdapter

        with self.assertRaises(ManifestError):
            PluginAdapter(manifest_toml="[plugin]\nversion = \"0.1.0\"\n")


if __name__ == "__main__":
    unittest.main()
