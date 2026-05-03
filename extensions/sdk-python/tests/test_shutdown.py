"""Phase 31.4 — shutdown lifecycle test."""

import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

SDK_ROOT = Path(__file__).resolve().parent.parent

DRIVER_SHUTDOWN_HANDLER = """
import asyncio
import sys
from nexo_plugin_sdk import PluginAdapter

MANIFEST = '''
[plugin]
id = "shut_plugin"
version = "0.1.0"
name = "Shut"
description = "fixture"
min_nexo_version = ">=0.1.0"
'''

shutdown_count = {"n": 0}

async def on_shutdown():
    shutdown_count["n"] += 1
    sys.stderr.write(f"shutdown_handler invoked count={shutdown_count['n']}\\n")
    sys.stderr.flush()

async def main():
    adapter = PluginAdapter(manifest_toml=MANIFEST, on_shutdown=on_shutdown)
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


class ShutdownTests(unittest.TestCase):
    def test_shutdown_request_replies_and_exits(self):
        proc = spawn_driver(DRIVER_SHUTDOWN_HANDLER)
        try:
            req = (
                json.dumps({"jsonrpc": "2.0", "id": 5, "method": "shutdown"}) + "\n"
            ).encode("utf-8")
            stdout, stderr = proc.communicate(input=req, timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
        self.assertEqual(proc.returncode, 0, f"non-zero exit: stderr={stderr!r}")
        lines = [l for l in stdout.decode("utf-8").splitlines() if l.strip()]
        first = json.loads(lines[0])
        self.assertEqual(first["id"], 5)
        self.assertTrue(first["result"]["ok"])
        self.assertIn(
            "shutdown_handler invoked count=1",
            stderr.decode("utf-8"),
            "on_shutdown must be awaited before reply",
        )


if __name__ == "__main__":
    unittest.main()
