<?php

declare(strict_types=1);

const FIXTURE = __DIR__ . '/fixtures/lifecycle-plugin.php';

function fail(string $msg): never
{
    fwrite(STDERR, "FAIL: $msg\n");
    exit(1);
}

// ── test 1: run_twice_throws_PluginError ──────────────────────
$proc = proc_open(
    ['php', FIXTURE],
    [0 => ['pipe', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']],
    $pipes,
);
if (!is_resource($proc)) {
    fail('proc_open failed');
}
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
$code = proc_close($proc);

if ($code !== 0) {
    fail("run_twice: expected exit 0, got $code stdout=$stdout stderr=$stderr");
}
if (!str_contains($stdout, 'LIFECYCLE_TEST_OK')) {
    fail("run_twice: sentinel missing stdout=$stdout stderr=$stderr");
}
fwrite(STDOUT, "ok 1 - run_twice_throws_PluginError\n");

exit(0);
