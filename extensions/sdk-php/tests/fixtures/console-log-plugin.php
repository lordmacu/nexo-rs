<?php

declare(strict_types=1);

/**
 * Fixture that calls `echo` AFTER PluginAdapter is constructed.
 * The default-on stdout guard must divert the line to stderr
 * tagged with STDOUT_GUARD_MARKER, keeping the JSON-RPC stream
 * clean for the host.
 */

require __DIR__ . '/../../vendor/autoload.php';

use Nexo\Plugin\Sdk\PluginAdapter;

$MANIFEST = <<<'TOML'
[plugin]
id = "noisy_plugin"
version = "0.1.0"
name = "Noisy"
description = "fixture"
min_nexo_version = ">=0.1.0"
TOML;

$adapter = new PluginAdapter([
    'manifestToml' => $MANIFEST,
    'handleProcessSignals' => false,
]);

// Trigger the guard: this line is NOT valid JSON, so it must
// be diverted to stderr.
echo "hello-from-noisy-plugin\n";

$adapter->run();
