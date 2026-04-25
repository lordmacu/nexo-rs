#!/usr/bin/env python3
"""Phase 19 — sample stdio poller extension.

Wire protocol (JSON-RPC 2.0 over stdin/stdout, one message per line):

  initialize { name, version }                -> handshake
  poll_tick  { kind, job_id, agent_id,        -> {
               cursor, config, now }              items_seen,
                                                 items_dispatched,
                                                 deliver: [...],
                                                 next_cursor,
                                                 next_interval_secs
                                               }

Error codes (returned as JSON-RPC error.code):
  -32001  Transient   network blip / 5xx — runner backs off
  -32002  Permanent   token revoked / scope changed — runner pauses
  -32602  Config      bad config payload — runner kills the job

Replace the body of `tick_template_poll` with whatever polling logic
your extension needs. The runner gives you cursor persistence,
schedule/jitter, lease, circuit breaker, dispatch, telemetry, and
admin endpoints for free — your code only describes what to fetch
and what to send.
"""

from __future__ import annotations

import base64
import json
import sys
import time
from typing import Any


def jsonrpc_response(req_id: Any, result: Any) -> str:
    return json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result})


def jsonrpc_error(req_id: Any, code: int, message: str) -> str:
    return json.dumps(
        {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
    )


def emit(line: str) -> None:
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


# ─────────────────────────────────────────────────────────────────────
# YOUR LOGIC GOES HERE
# ─────────────────────────────────────────────────────────────────────

def tick_template_poll(params: dict) -> dict:
    """One tick of the `template_poll` kind.

    `params` shape:
        kind:     "template_poll"
        job_id:   str
        agent_id: str
        cursor:   str | None    # base64 url-safe of opaque bytes
        config:   dict           # the job's `config:` block
        now:      str            # RFC3339 timestamp
    """
    cfg = params.get("config") or {}
    deliver_to = (cfg.get("deliver") or {}).get("to") or "0"
    deliver_channel = (cfg.get("deliver") or {}).get("channel") or "telegram"

    # Decode cursor if present. Every tick we just bump a counter
    # and emit one message — replace with a real fetch.
    counter = 0
    cursor_b64 = params.get("cursor")
    if cursor_b64:
        try:
            counter = int(base64.urlsafe_b64decode(cursor_b64 + "==").decode())
        except Exception:
            counter = 0
    counter += 1

    return {
        "items_seen": 1,
        "items_dispatched": 1,
        "deliver": [
            {
                "channel": deliver_channel,
                "recipient": deliver_to,
                "payload": {"text": f"template_poll tick #{counter}"},
            }
        ],
        "next_cursor": base64.urlsafe_b64encode(
            str(counter).encode()
        ).rstrip(b"=").decode(),
        "next_interval_secs": None,
    }


# ─────────────────────────────────────────────────────────────────────
# Custom LLM tools shipped by this kind
# ─────────────────────────────────────────────────────────────────────

def list_template_poll_tools() -> list[dict]:
    """Return the per-kind tools the agent can call. The runtime
    fetches this once at boot via `poll_list_tools` and caches it
    in `Poller::custom_tools()`. Agent then sees them alongside
    the six generic `pollers_*` tools.
    """
    return [
        {
            "name": "template_poll_status",
            "description": (
                "Report the internal counter of the template_poll "
                "job without persisting state. Useful as a sanity "
                "check before pause/resume."
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Job id (matches pollers.yaml)",
                    }
                },
                "required": ["id"],
            },
        }
    ]


def call_template_poll_status(args: dict) -> dict:
    """Implementation for the `template_poll_status` tool. Replace
    with whatever introspection your real poller exposes."""
    job_id = args.get("id") or "<unspecified>"
    return {"ok": True, "job_id": job_id, "note": "stub status"}


# ─────────────────────────────────────────────────────────────────────
# JSON-RPC dispatch loop — leave alone
# ─────────────────────────────────────────────────────────────────────


def dispatch(req: dict) -> str | None:
    method = req.get("method")
    req_id = req.get("id")
    params = req.get("params") or {}
    try:
        if method == "initialize":
            return jsonrpc_response(
                req_id,
                {
                    "name": "template-poller-python",
                    "version": "0.1.0",
                    "capabilities": {"pollers": ["template_poll"]},
                },
            )
        if method == "shutdown":
            return jsonrpc_response(req_id, {})
        if method == "poll_tick":
            kind = params.get("kind")
            if kind == "template_poll":
                return jsonrpc_response(req_id, tick_template_poll(params))
            return jsonrpc_error(req_id, -32602, f"unknown kind '{kind}'")
        if method == "poll_list_tools":
            kind = params.get("kind")
            if kind == "template_poll":
                return jsonrpc_response(req_id, list_template_poll_tools())
            return jsonrpc_response(req_id, [])
        if method == "poll_tool_call":
            kind = params.get("kind")
            tool_name = params.get("tool_name")
            args = params.get("args") or {}
            if kind == "template_poll" and tool_name == "template_poll_status":
                return jsonrpc_response(req_id, call_template_poll_status(args))
            return jsonrpc_error(
                req_id,
                -32601,
                f"unknown tool '{tool_name}' for kind '{kind}'",
            )
        return jsonrpc_error(req_id, -32601, f"method not found: {method}")
    except Exception as exc:  # noqa: BLE001
        return jsonrpc_error(req_id, -32001, f"transient: {exc}")


def main() -> None:
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            req = json.loads(raw)
        except json.JSONDecodeError as exc:
            emit(jsonrpc_error(None, -32700, f"parse error: {exc}"))
            continue
        resp = dispatch(req)
        if resp is not None:
            emit(resp)


if __name__ == "__main__":
    main()
