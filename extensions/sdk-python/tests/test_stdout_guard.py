"""Tests for the stdout guard (sub-phase 31.4.c).

Unit-level: exercise the guard module directly. Subprocess-level
divert/passthrough coverage lives in test_dispatch.py (the
console-print fixture plugin).
"""

import io
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

from nexo_plugin_sdk import stdout_guard

SDK_ROOT = Path(__file__).resolve().parent.parent


class StdoutGuardModuleTests(unittest.TestCase):
    def tearDown(self):
        # Never leave the guard installed across tests — it would
        # wrap the unittest runner's stdout.
        stdout_guard.uninstall_stdout_guard()

    def test_marker_sentinel(self):
        self.assertEqual(stdout_guard.STDOUT_GUARD_MARKER, "[stdout-guard]")

    def test_install_uninstall_roundtrip(self):
        real = sys.stdout
        self.assertFalse(stdout_guard.is_stdout_guard_installed())
        self.assertIsNone(stdout_guard.original_stdout())

        stdout_guard.install_stdout_guard()
        self.assertTrue(stdout_guard.is_stdout_guard_installed())
        self.assertIs(stdout_guard.original_stdout(), real)
        self.assertIsNot(sys.stdout, real)

        stdout_guard.uninstall_stdout_guard()
        self.assertFalse(stdout_guard.is_stdout_guard_installed())
        self.assertIs(sys.stdout, real)
        self.assertIsNone(stdout_guard.original_stdout())

    def test_install_is_idempotent(self):
        real = sys.stdout
        stdout_guard.install_stdout_guard()
        wrapped_once = sys.stdout
        stdout_guard.install_stdout_guard()  # must not double-wrap
        self.assertIs(sys.stdout, wrapped_once)
        self.assertIs(stdout_guard.original_stdout(), real)
        stdout_guard.uninstall_stdout_guard()
        self.assertIs(sys.stdout, real)

    def test_uninstall_when_not_installed_is_noop(self):
        real = sys.stdout
        stdout_guard.uninstall_stdout_guard()  # should not raise
        self.assertIs(sys.stdout, real)


class GuardWriterTests(unittest.TestCase):
    def test_diverts_non_json_keeps_json(self):
        target = io.StringIO()
        writer = stdout_guard._GuardWriter(target)
        with mock.patch.object(sys, "stderr", new=io.StringIO()) as fake_err:
            writer.write("hello world\n")
            writer.write('{"jsonrpc":"2.0","id":1,"result":{}}\n')
            writer.write("")  # blank line tolerated, forwarded
            writer.write("\n")
        self.assertEqual(
            target.getvalue(), '{"jsonrpc":"2.0","id":1,"result":{}}\n\n'
        )
        self.assertIn("[stdout-guard] hello world", fake_err.getvalue())
        self.assertNotIn("jsonrpc", fake_err.getvalue())

    def test_print_style_chunked_write(self):
        # print("x") issues write("x") then write("\n").
        target = io.StringIO()
        writer = stdout_guard._GuardWriter(target)
        with mock.patch.object(sys, "stderr", new=io.StringIO()) as fake_err:
            writer.write("debug line")
            self.assertEqual(target.getvalue(), "")  # nothing until newline
            self.assertEqual(fake_err.getvalue(), "")
            writer.write("\n")
        self.assertEqual(target.getvalue(), "")
        self.assertIn("[stdout-guard] debug line", fake_err.getvalue())

    def test_flush_partial_emits_buffered_tail(self):
        target = io.StringIO()
        writer = stdout_guard._GuardWriter(target)
        writer.write("partial without newline")
        self.assertEqual(target.getvalue(), "")
        writer.flush_partial()
        self.assertEqual(target.getvalue(), "partial without newline")

    def test_delegates_attrs_to_original(self):
        target = io.StringIO()
        writer = stdout_guard._GuardWriter(target)
        # io.StringIO has no `encoding`, but it has `getvalue` — make
        # sure unknown attrs route through to the wrapped object.
        self.assertTrue(callable(writer.getvalue))
        self.assertTrue(writer.writable())
        self.assertFalse(writer.readable())


DRIVER_PRINTING_HANDLER = """
import asyncio
import sys
from nexo_plugin_sdk import PluginAdapter, Event

MANIFEST = '''
[plugin]
id = "printer_plugin"
version = "0.1.0"
name = "Printer"
description = "fixture"
min_nexo_version = ">=0.1.0"
'''

async def on_event(topic, event, broker):
    print("noisy debug from handler")            # would corrupt the JSON-RPC stream without the guard
    sys.stdout.write("more noise via stdout.write\\n")
    out = Event.new("plugin.inbound.echo", "printer_plugin", {"echoed": event.payload})
    await broker.publish("plugin.inbound.echo", out)

async def main():
    adapter = PluginAdapter(manifest_toml=MANIFEST, on_event=on_event)
    await adapter.run()

asyncio.run(main())
"""


class StdoutGuardSubprocessTests(unittest.TestCase):
    def _spawn(self):
        env = dict(os.environ)
        env["PYTHONPATH"] = str(SDK_ROOT) + os.pathsep + env.get("PYTHONPATH", "")
        return subprocess.Popen(
            [sys.executable, "-c", DRIVER_PRINTING_HANDLER],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )

    def test_handler_print_diverted_blessed_frame_clean(self):
        proc = self._spawn()
        try:
            ev = (
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "method": "broker.event",
                        "params": {
                            "topic": "plugin.outbound.x",
                            "event": {"topic": "plugin.outbound.x", "source": "host", "payload": {"a": 1}},
                        },
                    }
                )
                + "\n"
            ).encode("utf-8")
            shutdown = (json.dumps({"jsonrpc": "2.0", "id": 9, "method": "shutdown"}) + "\n").encode("utf-8")
            stdout, stderr = proc.communicate(input=ev + shutdown, timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
        self.assertEqual(proc.returncode, 0, f"non-zero exit; stderr={stderr!r}")
        lines = [l for l in stdout.decode("utf-8").splitlines() if l.strip()]
        for line in lines:
            json.loads(line)  # every stdout line must be valid JSON — guard kept the noise out
        methods = [json.loads(l).get("method") for l in lines]
        self.assertIn("broker.publish", methods, f"blessed publish frame missing; stdout={lines}")
        err = stderr.decode("utf-8")
        self.assertIn("[stdout-guard] noisy debug from handler", err, f"stderr={err!r}")
        self.assertIn("[stdout-guard] more noise via stdout.write", err, f"stderr={err!r}")


if __name__ == "__main__":
    unittest.main()
