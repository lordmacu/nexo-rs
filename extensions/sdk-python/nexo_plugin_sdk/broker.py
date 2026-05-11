"""Child-side broker handle.

Plugin authors call ``broker.publish(topic, event)`` to emit
notifications back to the daemon. Topics MUST appear on the
manifest's ``[[plugin.channels.register]]`` allowlist or the
daemon drops the message with a warn log (defense in depth on
the host side).
"""

import asyncio
import sys
from typing import TextIO

from . import wire
from .events import Event


class BrokerSender:
    """Write-only handle to the daemon's broker.

    Wraps an injected line-writer (the captured *original* stdout,
    so blessed JSON-RPC frames bypass the stdout guard) behind an
    async lock so concurrent handler tasks do not interleave
    half-written frames.
    """

    def __init__(self, write_lock: asyncio.Lock, writer: TextIO | None = None) -> None:
        self._lock = write_lock
        # Default to bare ``sys.stdout`` for callers (mainly tests)
        # that construct a BrokerSender without going through the
        # adapter. The adapter always injects the captured original.
        self._writer: TextIO = writer if writer is not None else sys.stdout

    async def publish(self, topic: str, event: Event) -> None:
        line = wire.serialize_frame(
            wire.build_notification(
                "broker.publish", {"topic": topic, "event": event.to_json()}
            )
        )
        async with self._lock:
            self._writer.write(line)
            self._writer.flush()
