"""Phase 31.4 — minimal TOML reader for `nexo-plugin.toml`.

Validates only the fields the SDK needs at startup
(`plugin.id` + `plugin.version`). The daemon performs full
schema validation on the manifest at boot.
"""

from typing import Any

try:
    import tomllib  # Python ≥ 3.11
except ImportError:  # pragma: no cover - 3.10 fallback
    import tomli as tomllib  # type: ignore[no-redef]

from .errors import ManifestError


def read_manifest(toml_text: str) -> dict[str, Any]:
    """Parse manifest TOML, return the full document dict.

    Raises:
        ManifestError: parse failure or missing `plugin.id` /
        `plugin.version`.
    """
    try:
        data = tomllib.loads(toml_text)
    except Exception as e:  # tomllib.TOMLDecodeError on 3.11+
        raise ManifestError(f"manifest parse failed: {e}") from e

    plugin = data.get("plugin")
    if not isinstance(plugin, dict):
        raise ManifestError("manifest is missing the [plugin] section")

    plugin_id = plugin.get("id")
    if not isinstance(plugin_id, str) or not plugin_id:
        raise ManifestError("manifest is missing required string `plugin.id`")

    version = plugin.get("version")
    if not isinstance(version, str) or not version:
        raise ManifestError("manifest is missing required string `plugin.version`")

    return data
