"""JSON-RPC 2.0 frame helpers + wire constants.

Mirrors the TypeScript counterpart in
``extensions/sdk-typescript/src/wire.ts`` and the PHP counterpart
in ``extensions/sdk-php/src/Wire.php``. Frames are serialised as
one JSON object per line terminated by ``\\n`` — the host's reader
and the child's reader both line-buffer (see ``nexo-plugin-contract.md``).
"""

import json
from typing import Any

JSONRPC_VERSION = "2.0"

#: Maximum byte length of a single inbound JSON-RPC frame the SDK
#: accepts. Frames larger than this are rejected with a ``WireError``
#: log so an adversarial host cannot OOM the plugin via one huge
#: line. Matches ``MAX_FRAME_BYTES`` in the TypeScript and PHP SDKs.
MAX_FRAME_BYTES = 1 << 20  # 1 MiB


def serialize_frame(frame: dict[str, Any]) -> str:
    """Serialise a frame to a single line terminated by ``\\n``."""
    return json.dumps(frame) + "\n"


def build_response(req_id: Any, result: dict[str, Any]) -> dict[str, Any]:
    """Build a JSON-RPC 2.0 success response frame."""
    return {"jsonrpc": JSONRPC_VERSION, "id": req_id, "result": result}


def build_error_response(req_id: Any, code: int, message: str) -> dict[str, Any]:
    """Build a JSON-RPC 2.0 error response frame."""
    return {
        "jsonrpc": JSONRPC_VERSION,
        "id": req_id,
        "error": {"code": code, "message": message},
    }


def build_notification(method: str, params: dict[str, Any]) -> dict[str, Any]:
    """Build a JSON-RPC 2.0 notification frame (no ``id``)."""
    return {"jsonrpc": JSONRPC_VERSION, "method": method, "params": params}
