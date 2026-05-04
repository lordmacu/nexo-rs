<?php

declare(strict_types=1);

require __DIR__ . '/../vendor/autoload.php';

use Nexo\Plugin\Sdk\Manifest;
use Nexo\Plugin\Sdk\ManifestError;

function fail(string $msg): never
{
    fwrite(STDERR, "FAIL: $msg\n");
    exit(1);
}

// ── test 1: missing_id_throws_ManifestError_with_field ─────────
try {
    Manifest::parse(<<<'TOML'
[plugin]
version = "0.1.0"
name = "x"
description = "y"
TOML);
    fail('missing_id: expected ManifestError');
} catch (ManifestError $e) {
    if ($e->field !== 'plugin.id') {
        fail('missing_id: expected field=plugin.id, got ' . var_export($e->field, true));
    }
}
fwrite(STDOUT, "ok 1 - missing_id_throws_ManifestError_with_field\n");

// ── test 2: invalid_toml_throws_ManifestError ──────────────────
try {
    Manifest::parse('[[[unterminated');
    fail('invalid_toml: expected ManifestError');
} catch (ManifestError $e) {
    // pass
}
fwrite(STDOUT, "ok 2 - invalid_toml_throws_ManifestError\n");

// ── test 3: id_regex_violation_throws_ManifestError ────────────
try {
    Manifest::parse(<<<'TOML'
[plugin]
id = "Bad-Id"
version = "0.1.0"
name = "x"
description = "y"
TOML);
    fail('id_regex: expected ManifestError');
} catch (ManifestError $e) {
    if ($e->field !== 'plugin.id') {
        fail('id_regex: expected field=plugin.id, got ' . var_export($e->field, true));
    }
}
fwrite(STDOUT, "ok 3 - id_regex_violation_throws_ManifestError\n");

exit(0);
