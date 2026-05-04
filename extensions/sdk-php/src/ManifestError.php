<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — raised by `Manifest::parse()` and the
 * `PluginAdapter` constructor when `nexo-plugin.toml` is
 * malformed or required fields are missing.
 */

namespace Nexo\Plugin\Sdk;

final class ManifestError extends PluginError
{
    public function __construct(
        string $message,
        public readonly ?string $field = null,
    ) {
        parent::__construct($message);
    }
}
