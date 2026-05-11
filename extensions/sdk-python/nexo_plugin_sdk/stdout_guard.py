"""Defensive guard on ``sys.stdout`` that intercepts every write,
line-buffers, and only forwards lines that successfully ``json.loads``.
Non-JSON lines are diverted to ``sys.stderr`` tagged with the
``STDOUT_GUARD_MARKER`` sentinel.

Why: the daemon parses the plugin's stdout as newline-delimited
JSON-RPC frames (see ``nexo-plugin-contract.md``). Any stray write —
``print("hello")`` from the plugin author's code, a chatty dependency
banner, a debug print that slipped past review — would corrupt the
parser mid-stream with no recovery path. The guard converts those
mistakes from "fatal disconnect" into "tagged stderr line".

The blessed write path (``BrokerSender`` / ``PluginAdapter``) writes
through the *captured original* stdout (see :func:`original_stdout`),
so its frames never even reach the guard — and they are valid JSON
anyway, so they would pass through unchanged if they did.

Mirrors the TypeScript counterpart in
``extensions/sdk-typescript/src/stdout-guard.ts``. Python has no
``ob_start`` equivalent (that is the PHP SDK's mechanism), so this
swaps ``sys.stdout`` for a line-buffering proxy object.

Limitation: a C extension or subprocess that writes to file
descriptor 1 directly bypasses this proxy — Python's ``sys.stdout``
swap only intercepts the text-stream API. Plugin authors who need
stdout output should use ``print()`` / ``sys.stdout.write()``; those
are guarded.
"""

import json
import sys
from typing import TextIO

STDOUT_GUARD_MARKER = "[stdout-guard]"

_installed = False
_original: TextIO | None = None
_active: "_GuardWriter | None" = None


def _is_json_line(line: str) -> bool:
    # Empty lines tolerated — trailing newlines and blank separators
    # inside an NDJSON stream do not corrupt parsers.
    if line == "":
        return True
    try:
        json.loads(line)
        return True
    except ValueError:
        return False


class _GuardWriter:
    """Text-stream proxy. Buffers writes, forwards complete JSON lines
    to the wrapped original stream, diverts everything else to stderr.

    Attributes not defined here (``encoding``, ``errors``, ``name``,
    ``mode``, ``closed``, ``buffer`` …) delegate to the wrapped stream
    via ``__getattr__`` so libraries that introspect ``sys.stdout``
    keep working.
    """

    def __init__(self, original: TextIO) -> None:
        self._original = original
        self._buf = ""

    def write(self, text: object) -> int:
        if not isinstance(text, str):
            text = (
                text.decode("utf-8", "replace")
                if isinstance(text, (bytes, bytearray))
                else str(text)
            )
        self._buf += text
        written = len(text)
        while True:
            idx = self._buf.find("\n")
            if idx == -1:
                break
            line = self._buf[:idx]
            self._buf = self._buf[idx + 1 :]
            if _is_json_line(line):
                self._original.write(line + "\n")
            else:
                sys.stderr.write(f"{STDOUT_GUARD_MARKER} {line}\n")
                sys.stderr.flush()
        return written

    def writelines(self, lines: object) -> None:
        for line in lines:  # type: ignore[union-attr]
            self.write(line)

    def flush(self) -> None:
        # A partial line (no trailing newline yet) stays buffered —
        # an NDJSON frame is not complete until its newline arrives.
        self._original.flush()

    def flush_partial(self) -> None:
        """Emit any buffered partial line raw to the original stream.

        Called by :func:`uninstall_stdout_guard` so debug output
        written without a trailing newline is not lost across an
        uninstall.
        """
        if self._buf:
            self._original.write(self._buf)
            self._buf = ""

    def writable(self) -> bool:
        return True

    def readable(self) -> bool:
        return False

    def seekable(self) -> bool:
        return False

    def fileno(self) -> int:
        return self._original.fileno()

    def isatty(self) -> bool:
        return self._original.isatty()

    def __getattr__(self, name: str) -> object:
        # Only reached for attributes not defined above. Guard against
        # access before __init__ wired _original.
        original = self.__dict__.get("_original")
        if original is None:
            raise AttributeError(name)
        return getattr(original, name)


def install_stdout_guard() -> None:
    """Swap ``sys.stdout`` for the guard proxy. Idempotent."""
    global _installed, _original, _active
    if _installed:
        return
    _original = sys.stdout
    _active = _GuardWriter(_original)
    sys.stdout = _active  # type: ignore[assignment]
    _installed = True


def uninstall_stdout_guard() -> None:
    """Restore the original ``sys.stdout``. Idempotent."""
    global _installed, _original, _active
    if not _installed or _original is None:
        return
    if _active is not None:
        _active.flush_partial()
    sys.stdout = _original
    _original = None
    _active = None
    _installed = False


def is_stdout_guard_installed() -> bool:
    return _installed


def original_stdout() -> TextIO | None:
    """The captured pre-install ``sys.stdout``, or ``None`` if the
    guard is not installed. The blessed write path uses this so
    JSON-RPC frames bypass the guard."""
    return _original
