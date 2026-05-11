"""Python SDK for nexo subprocess plugins.

Child-side counterpart to the host's plugin protocol
(``nexo-plugin-contract.md``). Public API mirrors the Rust SDK in
``crates/microapp-sdk/`` and the TypeScript SDK in
``extensions/sdk-typescript/``.
"""

from .adapter import EventHandler, PluginAdapter, ShutdownHandler
from .broker import BrokerSender
from .errors import ManifestError, PluginError, WireError
from .events import Event
from .manifest import read_manifest
from .stdout_guard import (
    STDOUT_GUARD_MARKER,
    install_stdout_guard,
    is_stdout_guard_installed,
    uninstall_stdout_guard,
)
from .wire import (
    JSONRPC_VERSION,
    MAX_FRAME_BYTES,
    build_error_response,
    build_notification,
    build_response,
    serialize_frame,
)

__all__ = [
    # Core
    "PluginAdapter",
    "BrokerSender",
    "Event",
    "EventHandler",
    "ShutdownHandler",
    # Errors
    "PluginError",
    "ManifestError",
    "WireError",
    # Manifest
    "read_manifest",
    # Stdout guard
    "install_stdout_guard",
    "uninstall_stdout_guard",
    "is_stdout_guard_installed",
    "STDOUT_GUARD_MARKER",
    # Wire helpers
    "JSONRPC_VERSION",
    "MAX_FRAME_BYTES",
    "serialize_frame",
    "build_response",
    "build_error_response",
    "build_notification",
]

__version__ = "0.2.0"
