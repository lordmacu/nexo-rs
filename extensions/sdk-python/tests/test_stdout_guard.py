"""Tests for the stdout guard (sub-phase 31.4.c).

Unit-level: exercise the guard module directly. Subprocess-level
divert/passthrough coverage lives in test_dispatch.py (the
console-print fixture plugin).
"""

import io
import sys
import unittest
from unittest import mock

from nexo_plugin_sdk import stdout_guard


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


if __name__ == "__main__":
    unittest.main()
