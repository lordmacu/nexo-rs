"""Lifecycle tests for the Python plugin SDK (sub-phase 31.4.c).

Black-box tests: spawn a fresh `python3` child running a tiny
driver that imports `nexo_plugin_sdk.PluginAdapter`, feed it
JSON-RPC frames / signals, assert exit code + stderr markers.
"""

import json
import os
import subprocess
import sys
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


if __name__ == "__main__":
    unittest.main()
