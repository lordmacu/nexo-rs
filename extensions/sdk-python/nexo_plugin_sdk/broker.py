"""Phase 31.4 — child-side broker handle.

Plugin authors call ``broker.publish(topic, event)`` to emit
notifications back to the daemon. Topics MUST appear on the
manifest's ``[[plugin.channels.register]]`` allowlist or the
daemon drops the message with a warn log (defense in depth on
the host side).
"""

import asyncio
import json
import sys
from typing import Any

from .events import Event


class BrokerSender:
    """Write-only handle to the daemon's broker. Wraps stdout
    behind an async lock so concurrent handler tasks do not
    interleave half-written JSON-RPC frames.
    """

    def __init__(self, write_lock: asyncio.Lock) -> None:
        self._lock = write_lock

    async def publish(self, topic: str, event: Event) -> None:
        notification: dict[str, Any] = {
            "jsonrpc": "2.0",
            "method": "broker.publish",
            "params": {"topic": topic, "event": event.to_json()},
        }
        line = json.dumps(notification) + "\n"
        async with self._lock:
            sys.stdout.write(line)
            sys.stdout.flush()
