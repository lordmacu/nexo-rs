"""Phase 31.4 — child-side dispatch loop (robustness defaults: 31.4.c).

Mirrors the Rust counterpart in
`crates/microapp-sdk/src/plugin.rs::PluginAdapter` and the
TypeScript counterpart in `extensions/sdk-typescript/src/adapter.ts`.
Reads JSON-RPC 2.0 newline-delimited frames from stdin, dispatches:

  - ``method == "initialize"`` (request) → reply with manifest +
    server_version.
  - ``method == "broker.event"`` (notification) → spawn a
    detached task running ``on_event`` so the reader continues
    polling stdin while the handler awaits its own broker
    interactions (mirrors the self-deadlock fix from Phase 81.15.c).
  - ``method == "shutdown"`` (request) → drain in-flight tasks,
    reply ``{"ok": true}``, invoke ``on_shutdown`` if set, exit
    the loop.
  - Anything else with an id → reply error ``-32601 method not found``.
  - Anything else without an id (notification) → silently ignore
    (JSON-RPC 2.0 §4.1).

Wire format: ``nexo-plugin-contract.md``.
"""

import asyncio
import json
import signal
import sys
from typing import Any, Awaitable, Callable

from . import stdout_guard, wire
from .broker import BrokerSender
from .errors import PluginError, WireError
from .events import Event
from .manifest import read_manifest

EventHandler = Callable[[str, Event, BrokerSender], Awaitable[None]]
ShutdownHandler = Callable[[], Awaitable[None]]

# Signals that trigger a graceful shutdown (drain in-flight handlers,
# then exit 0). Mirrors the TS SDK's SIGTERM/SIGINT handling.
_SHUTDOWN_SIGNALS = (signal.SIGTERM, signal.SIGINT)


def _safe_remove_asyncio_signal(loop: asyncio.AbstractEventLoop, sig: int) -> None:
    try:
        loop.remove_signal_handler(sig)
    except (ValueError, RuntimeError):
        pass


def _safe_restore_signal(sig: int, previous: Any) -> None:
    # signal.signal returns SIG_DFL/SIG_IGN/a callable, or None when a
    # non-Python handler was installed (which we cannot restore).
    if previous is None:
        return
    try:
        signal.signal(sig, previous)
    except (ValueError, OSError, TypeError):
        pass


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
        enable_stdout_guard: bool = True,
        max_frame_bytes: int = wire.MAX_FRAME_BYTES,
        handle_process_signals: bool = True,
    ) -> None:
        # Parse + validate the manifest first — a failed construction
        # must not leave a dangling stdout guard installed.
        self._manifest = read_manifest(manifest_toml)
        self._server_version = server_version
        self._on_event = on_event
        self._on_shutdown = on_shutdown
        self._enable_stdout_guard = enable_stdout_guard
        self._max_frame_bytes = max_frame_bytes
        self._handle_process_signals = handle_process_signals
        # Single-shot guard — run() may be called at most once.
        self._started = False

        if enable_stdout_guard:
            stdout_guard.install_stdout_guard()
        # Blessed frames (initialize/shutdown replies, broker.publish)
        # write through the captured *original* stdout so they bypass
        # the guard entirely.
        self._stdout = stdout_guard.original_stdout() or sys.stdout

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

    def _install_signal_handlers(
        self, loop: asyncio.AbstractEventLoop, stop_event: asyncio.Event
    ) -> list[Callable[[], None]]:
        """Register SIGTERM/SIGINT → ``stop_event.set()``. Returns a
        list of cleanup callbacks to undo the registrations."""
        cleanups: list[Callable[[], None]] = []
        if not self._handle_process_signals:
            return cleanups
        for sig in _SHUTDOWN_SIGNALS:
            try:
                loop.add_signal_handler(sig, stop_event.set)
                cleanups.append(
                    (lambda s: lambda: _safe_remove_asyncio_signal(loop, s))(sig)
                )
            except (NotImplementedError, RuntimeError, ValueError):
                # add_signal_handler is unavailable (Windows
                # ProactorEventLoop, or not the main thread). Fall
                # back to the threadsafe-callback bridge.
                try:
                    previous = signal.signal(
                        sig,
                        lambda *_a: loop.call_soon_threadsafe(stop_event.set),
                    )
                except (ValueError, OSError, RuntimeError):
                    sys.stderr.write(
                        f"plugin: signal {sig} not installable on this "
                        "platform; graceful shutdown on signal disabled\n"
                    )
                    sys.stderr.flush()
                    continue
                cleanups.append(
                    (lambda s, p: lambda: _safe_restore_signal(s, p))(sig, previous)
                )
        return cleanups

    async def run(self) -> None:
        if self._started:
            raise PluginError("PluginAdapter.run() already invoked")
        self._started = True

        loop = asyncio.get_running_loop()
        # Fully-async stdin reader — no threadpool, so signal-driven
        # cancellation is clean. The StreamReader limit is sized above
        # max_frame_bytes so the explicit byte-cap check below is the
        # one that fires for slightly oversized frames; truly absurd
        # frames hit the limit and are dropped by readline() itself.
        reader = asyncio.StreamReader(limit=self._max_frame_bytes + 64 * 1024)
        protocol = asyncio.StreamReaderProtocol(reader)
        transport, _ = await loop.connect_read_pipe(lambda: protocol, sys.stdin)

        stop_event = asyncio.Event()
        signal_cleanups = self._install_signal_handlers(loop, stop_event)
        try:
            while not stop_event.is_set():
                read_task = asyncio.ensure_future(reader.readline())
                stop_task = asyncio.ensure_future(stop_event.wait())
                try:
                    await asyncio.wait(
                        {read_task, stop_task},
                        return_when=asyncio.FIRST_COMPLETED,
                    )
                finally:
                    if not read_task.done():
                        read_task.cancel()
                    if not stop_task.done():
                        stop_task.cancel()
                if stop_event.is_set():
                    # Graceful shutdown via signal — drop any line just
                    # read; the host is tearing us down. In-flight
                    # handlers are drained in the finally below.
                    break
                try:
                    line_bytes = read_task.result()
                except ValueError as e:
                    # readline() drops the oversized line from its
                    # buffer before raising — safe to keep dispatching.
                    err = WireError(f"oversized inbound frame dropped: {e}")
                    sys.stderr.write(f"plugin: {err}\n")
                    sys.stderr.flush()
                    continue
                except asyncio.CancelledError:
                    break
                if not line_bytes:
                    # EOF — host closed stdin.
                    break
                if len(line_bytes) > self._max_frame_bytes:
                    err = WireError(
                        f"inbound frame {len(line_bytes)} bytes exceeds "
                        f"max_frame_bytes {self._max_frame_bytes}"
                    )
                    sys.stderr.write(f"plugin: {err}\n")
                    sys.stderr.flush()
                    continue
                line = line_bytes.decode("utf-8", "replace").strip()
                if not line:
                    continue
                stop = await self._handle_line(line)
                if stop:
                    break
        finally:
            for undo in signal_cleanups:
                undo()
            transport.close()
            await self._drain_inflight()
            if self._enable_stdout_guard:
                stdout_guard.uninstall_stdout_guard()

    async def _handle_line(self, line: str) -> bool:
        """Process one JSON-RPC frame. Returns True if the loop
        should stop (a ``shutdown`` request was handled)."""
        try:
            msg = json.loads(line)
        except json.JSONDecodeError as e:
            # Garbage line — log to stderr and continue. The host is
            # the source of truth; we never mutate the wire spec from
            # the child side.
            sys.stderr.write(f"plugin: malformed jsonrpc line: {e}\n")
            sys.stderr.flush()
            return False
        if not isinstance(msg, dict):
            sys.stderr.write("plugin: jsonrpc frame must be an object\n")
            sys.stderr.flush()
            return False
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
            return True
        elif req_id is not None:
            # Unknown request — JSON-RPC requires a reply.
            await self._send_error(req_id, -32601, "method not found")
        # Unknown notification (no id) — silently ignore per
        # JSON-RPC 2.0 §4.1 for fire-and-forget frames.
        return False

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
        line = wire.serialize_frame(wire.build_response(req_id, result))
        async with self._write_lock:
            self._stdout.write(line)
            self._stdout.flush()

    async def _send_error(self, req_id: Any, code: int, message: str) -> None:
        line = wire.serialize_frame(wire.build_error_response(req_id, code, message))
        async with self._write_lock:
            self._stdout.write(line)
            self._stdout.flush()
