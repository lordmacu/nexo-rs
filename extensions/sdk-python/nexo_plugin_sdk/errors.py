"""Phase 31.4 — exception types raised by the Python plugin SDK."""


class PluginError(Exception):
    """Base class for SDK errors."""


class ManifestError(PluginError):
    """`nexo-plugin.toml` failed to parse or required fields are missing."""


class WireError(PluginError):
    """Malformed JSON-RPC frame received from the host, or malformed
    payload sent by the plugin author's handler."""
