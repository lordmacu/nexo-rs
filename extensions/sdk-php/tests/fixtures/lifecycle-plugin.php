<?php

declare(strict_types=1);

/**
 * Lifecycle fixture: constructs PluginAdapter, calls run()
 * twice. Second call must throw PluginError; fixture writes a
 * sentinel on stdout and exits 0.
 *
 * Driven via a child process so the readline loop the SDK
 * starts in `run()` does not block the test runner's stdin.
 */

require __DIR__ . '/../../vendor/autoload.php';

use Nexo\Plugin\Sdk\PluginAdapter;
use Nexo\Plugin\Sdk\PluginError;

$MANIFEST = <<<'TOML'
[plugin]
id = "lifecycle_plugin"
version = "0.1.0"
name = "Lifecycle"
description = "fixture"
min_nexo_version = ">=0.1.0"
TOML;

$adapter = new PluginAdapter([
    'manifestToml' => $MANIFEST,
    'enableStdoutGuard' => false,
    'handleProcessSignals' => false,
]);

// First run() call would enter the readline loop forever since
// stdin from the test runner never emits EOF. We start it in a
// short-circuit fiber that we never resume — the `started` flag
// is set synchronously inside run(), so the second call below
// throws immediately.
$first = new \Fiber(function () use ($adapter): void {
    try {
        $adapter->run();
    } catch (\Throwable $e) {
        // Swallow — never resumed.
    }
});
$first->start();
// `started` flag is set synchronously inside run() before the
// scheduler loop begins; second invocation throws.

try {
    $adapter->run();
    fwrite(STDERR, "LIFECYCLE_TEST_FAIL: second run resolved instead of rejecting\n");
    exit(2);
} catch (PluginError $e) {
    if (str_contains($e->getMessage(), 'already invoked')) {
        fwrite(STDOUT, "LIFECYCLE_TEST_OK\n");
        exit(0);
    }
    fwrite(STDERR, "LIFECYCLE_TEST_FAIL: wrong message: " . $e->getMessage() . "\n");
    exit(3);
} catch (\Throwable $e) {
    fwrite(STDERR, "LIFECYCLE_TEST_FAIL: wrong type: " . get_class($e) . ': ' . $e->getMessage() . "\n");
    exit(4);
}
