"""Minimal TOML reader for `nexo-plugin.toml`.

Validates only the fields the SDK needs at startup (`plugin.id` —
incl. the ASCII-slug regex the host enforces — and `plugin.version`).
The daemon performs full schema validation on the manifest at boot.
"""

import re
from typing import Any

try:
    import tomllib  # Python ≥ 3.11
except ImportError:  # pragma: no cover - 3.10 fallback
    import tomli as tomllib  # type: ignore[no-redef]

from .errors import ManifestError

#: Same shape the host's `nexo-plugin-manifest` crate enforces and
#: the TypeScript SDK validates: `^[a-z][a-z0-9_]{0,31}$`.
PLUGIN_ID_REGEX = re.compile(r"^[a-z][a-z0-9_]{0,31}$")


def read_manifest(toml_text: str) -> dict[str, Any]:
    """Parse manifest TOML, return the full document dict.

    Raises:
        ManifestError: parse failure, missing `plugin.id` /
        `plugin.version`, or a `plugin.id` that violates
        :data:`PLUGIN_ID_REGEX`.
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
    if not PLUGIN_ID_REGEX.match(plugin_id):
        raise ManifestError(
            f'plugin.id "{plugin_id}" must match {PLUGIN_ID_REGEX.pattern}'
        )

    version = plugin.get("version")
    if not isinstance(version, str) or not version:
        raise ManifestError("manifest is missing required string `plugin.version`")

    return data
