"""Phase 31.4 — child-side dispatch loop.

Mirrors the Rust counterpart in
`crates/microapp-sdk/src/plugin.rs::PluginAdapter`. Reads
JSON-RPC 2.0 newline-delimited frames from stdin, dispatches:

  - ``method == "initialize"`` (request) → reply with manifest +
    server_version.
  - ``method == "broker.event"`` (notification) → spawn a
    detached task running ``on_event`` so the reader continues
    polling stdin while the handler awaits its own broker
    interactions (mirrors the self-deadlock fix from Phase 81.15.c).
  - ``method == "shutdown"`` (request) → reply ``{"ok": true}``,
    invoke ``on_shutdown`` if set, exit the loop.
  - Anything else → reply error ``-32601 method not found``.
"""

import asyncio
import json
import sys
from typing import Any, Awaitable, Callable

from .broker import BrokerSender
from .errors import WireError
from .events import Event
from .manifest import read_manifest

EventHandler = Callable[[str, Event, BrokerSender], Awaitable[None]]
ShutdownHandler = Callable[[], Awaitable[None]]

JSONRPC_VERSION = "2.0"


class PluginAdapter:
    """Wraps the JSON-RPC dispatch loop. Construct once with the
    manifest TOML body, then call ``await adapter.run()`` from
    your async entrypoint.
    """

    def __init__(
        self,
        *,
        manifest_toml: str,
        server_version: str = "0.1.0",
        on_event: EventHandler | None = None,
        on_shutdown: ShutdownHandler | None = None,
    ) -> None:
        self._manifest = read_manifest(manifest_toml)
        self._server_version = server_version
        self._on_event = on_event
        self._on_shutdown = on_shutdown
        self._write_lock = asyncio.Lock()
        self._broker = BrokerSender(self._write_lock)
        # Track in-flight handler tasks so shutdown can await them
        # before the loop returns. Without this, `asyncio.run`
        # would cancel mid-handler tasks on exit and the host would
        # observe truncated broker.publish frames.
        self._inflight: set[asyncio.Task[None]] = set()

    @property
    def manifest(self) -> dict[str, Any]:
        return self._manifest

    async def run(self) -> None:
        loop = asyncio.get_event_loop()
        while True:
            line = await loop.run_in_executor(None, sys.stdin.readline)
            if not line:
                # EOF — host closed stdin.
                break
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError as e:
                # Garbage line — log to stderr and continue. The
                # host is the source of truth; we never mutate the
                # wire spec from the child side.
                sys.stderr.write(f"plugin: malformed jsonrpc line: {e}\n")
                sys.stderr.flush()
                continue
            method = msg.get("method")
            req_id = msg.get("id")
            if method == "initialize":
                await self._reply_initialize(req_id)
            elif method == "broker.event":
                params = msg.get("params") or {}
                task = asyncio.create_task(self._dispatch_event(params))
                self._inflight.add(task)
                task.add_done_callback(self._inflight.discard)
            elif method == "shutdown":
                await self._drain_inflight()
                await self._reply_shutdown(req_id)
                break
            elif req_id is not None:
                # Unknown request — JSON-RPC requires a reply.
                await self._send_error(req_id, -32601, "method not found")
            # Unknown notification (no id) — silently ignore per
            # JSON-RPC 2.0 §4.1 for fire-and-forget frames.

    async def _drain_inflight(self) -> None:
        """Wait for outstanding handler tasks before exiting the
        loop. Daemon's supervisor gives plugins ~1s after a
        shutdown reply to flush state (see Phase 81.21), so we
        block here without a hard timeout — the host's SIGKILL
        path is the safety net for runaway handlers.
        """
        if not self._inflight:
            return
        await asyncio.gather(*list(self._inflight), return_exceptions=True)

    async def _reply_initialize(self, req_id: Any) -> None:
        result = {
            "manifest": self._manifest,
            "server_version": self._server_version,
        }
        await self._send_response(req_id, result)

    async def _reply_shutdown(self, req_id: Any) -> None:
        if self._on_shutdown is not None:
            try:
                await self._on_shutdown()
            except Exception as e:
                sys.stderr.write(f"plugin: on_shutdown raised: {e}\n")
                sys.stderr.flush()
        await self._send_response(req_id, {"ok": True})

    async def _dispatch_event(self, params: dict[str, Any]) -> None:
        if self._on_event is None:
            return
        try:
            topic = params.get("topic")
            raw_event = params.get("event") or {}
            if not isinstance(topic, str):
                raise WireError("broker.event params missing string `topic`")
            if not isinstance(raw_event, dict):
                raise WireError("broker.event params missing dict `event`")
            event = Event.from_json(raw_event)
            await self._on_event(topic, event, self._broker)
        except Exception as e:
            sys.stderr.write(f"plugin: on_event raised: {e}\n")
            sys.stderr.flush()

    async def _send_response(self, req_id: Any, result: dict[str, Any]) -> None:
        frame = {"jsonrpc": JSONRPC_VERSION, "id": req_id, "result": result}
        line = json.dumps(frame) + "\n"
        async with self._write_lock:
            sys.stdout.write(line)
            sys.stdout.flush()

    async def _send_error(self, req_id: Any, code: int, message: str) -> None:
        frame = {
            "jsonrpc": JSONRPC_VERSION,
            "id": req_id,
            "error": {"code": code, "message": message},
        }
        line = json.dumps(frame) + "\n"
        async with self._write_lock:
            sys.stdout.write(line)
            sys.stdout.flush()
