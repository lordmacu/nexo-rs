#!/usr/bin/env python3
"""Starter stdio JSON-RPC extension for agent-rs.

Protocol: line-delimited JSON-RPC 2.0.

Methods:
  - initialize   → returns { server_version, tools, hooks }
  - tools/list   → returns { tools }
  - tools/call   → dispatches on name; returns { ...result }
  - shutdown     → exits the process

Sample tools: ping (zero-arg), add (two numbers).
"""

import json
import sys
import time


TOOL_SCHEMAS = [
    {
        "name": "ping",
        "description": "Returns pong + unix timestamp of receipt",
        "input_schema": {"type": "object", "additionalProperties": False},
    },
    {
        "name": "add",
        "description": "Returns the sum of numbers a and b",
        "input_schema": {
            "type": "object",
            "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
            "required": ["a", "b"],
            "additionalProperties": False,
        },
    },
]


def tool_ping(_args):
    return {"pong": True, "received_at_unix": int(time.time())}


class InvalidArgs(ValueError):
    """Raised when tool arguments are shape-invalid. Maps to JSON-RPC -32602."""


def tool_add(args):
    # Strict shape validation before any work — makes the LLM see a
    # precise `-32602 Invalid params` instead of a generic -32000, so
    # retries converge faster.
    if not isinstance(args, dict):
        raise InvalidArgs("arguments must be a JSON object")
    for key in ("a", "b"):
        if key not in args:
            raise InvalidArgs(f"missing required field '{key}'")
    try:
        a = float(args["a"])
        b = float(args["b"])
    except (TypeError, ValueError) as exc:
        raise InvalidArgs(f"fields a,b must be numeric: {exc}") from exc
    return {"sum": a + b}


TOOLS = {"ping": tool_ping, "add": tool_add}


def write_result(req_id, result):
    sys.stdout.write(
        json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result}) + "\n"
    )
    sys.stdout.flush()


def write_error(req_id, code, message):
    sys.stdout.write(
        json.dumps(
            {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
        )
        + "\n"
    )
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as exc:
            write_error(None, -32700, f"parse error: {exc}")
            continue

        rid = req.get("id")
        method = req.get("method", "")
        params = req.get("params") or {}

        if method == "initialize":
            write_result(
                rid,
                {
                    "server_version": "template-python-0.1.0",
                    "tools": TOOL_SCHEMAS,
                    "hooks": ["before_message"],
                },
            )
        elif method == "tools/list":
            write_result(rid, {"tools": TOOL_SCHEMAS})
        elif method == "tools/call":
            name = params.get("name", "")
            args = params.get("arguments", {})
            tool = TOOLS.get(name)
            if tool is None:
                write_error(rid, -32601, f"unknown tool: {name}")
                continue
            try:
                write_result(rid, tool(args))
            except InvalidArgs as exc:
                # Semantic JSON-RPC error: client supplied bad arguments.
                # The LLM can see this and reformulate the call.
                write_error(rid, -32602, str(exc))
            except Exception as exc:
                # Unexpected failure inside the tool — log the traceback
                # to stderr (the host forwards it to its tracing
                # subscriber) and surface a generic internal error.
                import traceback
                print(
                    f"[template-python] tool `{name}` crashed:\n{traceback.format_exc()}",
                    file=sys.stderr,
                    flush=True,
                )
                write_error(rid, -32603, f"internal error: {exc}")
        elif method.startswith("hooks/"):
            hook_name = method[len("hooks/"):]
            print(f"[template-python] hook {hook_name}: {json.dumps(params)}",
                  file=sys.stderr, flush=True)
            # Minimal policy demo: on `before_message`, reject any
            # message whose `text` field contains a banned word.
            # Remove or soften this block for an observer-only hook.
            BANNED = ("__banned_token__",)
            if hook_name == "before_message":
                text = (params.get("text") or "").lower()
                if any(tok in text for tok in BANNED):
                    write_result(
                        rid,
                        {
                            "abort": True,
                            "reason": "template-python blocked: banned token in message",
                        },
                    )
                    continue
                # Rewrite demo: strip leading whitespace from `text`
                # via the `override` field so downstream hooks / the
                # agent see a cleaner value. No-op if `text` is
                # absent or not a string.
                if isinstance(params.get("text"), str) and params["text"] != params["text"].lstrip():
                    write_result(
                        rid,
                        {
                            "abort": False,
                            "override": {"text": params["text"].lstrip()},
                        },
                    )
                    continue
            write_result(rid, {"abort": False})
        elif method == "shutdown":
            if rid is not None:
                write_result(rid, {"ok": True})
            return
        else:
            write_error(rid, -32601, f"method not found: {method}")


if __name__ == "__main__":
    main()
