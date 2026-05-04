<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — manifest TOML parser.
 *
 * Validates only the fields the SDK needs at construction time.
 * The daemon performs full schema validation at boot, so this
 * stays minimal.
 */

namespace Nexo\Plugin\Sdk;

use Yosymfony\Toml\Toml;

final class Manifest
{
    private const PLUGIN_ID_REGEX = '/^[a-z][a-z0-9_]{0,31}$/';

    /**
     * Parse manifest TOML, return the full document array.
     *
     * @return array<string, mixed>
     * @throws ManifestError
     */
    public static function parse(string $toml): array
    {
        try {
            $raw = Toml::parse($toml);
        } catch (\Throwable $e) {
            throw new ManifestError('manifest TOML parse failed: ' . $e->getMessage());
        }

        if (!is_array($raw)) {
            throw new ManifestError('manifest must parse to a TOML table');
        }

        $plugin = $raw['plugin'] ?? null;
        if (!is_array($plugin)) {
            throw new ManifestError('manifest is missing the [plugin] section', 'plugin');
        }

        $id = self::requireString($plugin, 'id', 'plugin');
        if (preg_match(self::PLUGIN_ID_REGEX, $id) !== 1) {
            throw new ManifestError(
                "plugin.id \"$id\" must match " . self::PLUGIN_ID_REGEX,
                'plugin.id',
            );
        }
        self::requireString($plugin, 'version', 'plugin');
        self::requireString($plugin, 'name', 'plugin');
        self::requireString($plugin, 'description', 'plugin');

        return $raw;
    }

    /**
     * @param array<string, mixed> $obj
     * @throws ManifestError
     */
    private static function requireString(array $obj, string $field, string $ownerLabel): string
    {
        $value = $obj[$field] ?? null;
        if (!is_string($value) || $value === '') {
            throw new ManifestError(
                "$ownerLabel.$field is missing or not a non-empty string",
                "$ownerLabel.$field",
            );
        }
        return $value;
    }
}
