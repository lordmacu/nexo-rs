"""Phase 31.4 — Python SDK for nexo subprocess plugins.

Public API mirrors the Rust SDK in `crates/microapp-sdk/`.
"""

from .adapter import PluginAdapter
from .broker import BrokerSender
from .errors import ManifestError, PluginError, WireError
from .events import Event
from .manifest import read_manifest

__all__ = [
    "PluginAdapter",
    "BrokerSender",
    "Event",
    "PluginError",
    "ManifestError",
    "WireError",
    "read_manifest",
]

__version__ = "0.1.0"
