<?php

declare(strict_types=1);

require __DIR__ . '/../vendor/autoload.php';

use Nexo\Plugin\Sdk\StdoutGuard;

const NOISY_FIXTURE = __DIR__ . '/fixtures/console-log-plugin.php';

function fail(string $msg): never
{
    fwrite(STDERR, "FAIL: $msg\n");
    exit(1);
}

// ── test 1: idempotent_install ─────────────────────────────────
$wasInstalled = StdoutGuard::isInstalled();
StdoutGuard::install();
StdoutGuard::install(); // second call no-op
if (!StdoutGuard::isInstalled()) {
    fail('idempotent_install: guard should be installed');
}
StdoutGuard::uninstall();
if (StdoutGuard::isInstalled()) {
    fail('idempotent_install: guard should be uninstalled');
}
StdoutGuard::install();
StdoutGuard::uninstall();
if (StdoutGuard::isInstalled() !== $wasInstalled) {
    fail('idempotent_install: failed to restore initial state');
}
fwrite(STDOUT, "ok 1 - idempotent_install\n");

// ── test 2: diverts_non_json_echo_to_stderr ────────────────────
// The fixture calls `echo "hello-from-noisy-plugin\n"` BEFORE
// entering the dispatch loop. With the guard installed
// (default-on) the line goes to stderr tagged with the
// sentinel, NOT stdout.
$proc = proc_open(
    ['php', NOISY_FIXTURE],
    [0 => ['pipe', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']],
    $pipes,
);
if (!is_resource($proc)) {
    fail('diverts_non_json_echo: proc_open failed');
}
// Drive a complete handshake so the fixture exits.
fwrite($pipes[0], json_encode(['jsonrpc' => '2.0', 'id' => 1, 'method' => 'shutdown']) . "\n");
fclose($pipes[0]);
stream_set_blocking($pipes[1], false);
stream_set_blocking($pipes[2], false);
$stdout = '';
$stderr = '';
$deadline = microtime(true) + 10;
while (microtime(true) < $deadline) {
    $status = proc_get_status($proc);
    $stdout .= stream_get_contents($pipes[1]) ?: '';
    $stderr .= stream_get_contents($pipes[2]) ?: '';
    if (!$status['running']) {
        break;
    }
    usleep(20_000);
}
$stdout .= stream_get_contents($pipes[1]) ?: '';
$stderr .= stream_get_contents($pipes[2]) ?: '';
fclose($pipes[1]);
fclose($pipes[2]);
proc_close($proc);

if (str_contains($stdout, 'hello-from-noisy-plugin')) {
    fail('diverts_non_json_echo: non-JSON line leaked to stdout: ' . $stdout);
}
if (!str_contains($stderr, StdoutGuard::MARKER . ' hello-from-noisy-plugin')) {
    fail('diverts_non_json_echo: guarded line missing on stderr: ' . $stderr);
}
if (!str_contains($stdout, '"ok":true')) {
    fail('diverts_non_json_echo: shutdown reply missing from stdout: ' . $stdout . ' stderr=' . $stderr);
}
fwrite(STDOUT, "ok 2 - diverts_non_json_echo_to_stderr\n");

exit(0);
