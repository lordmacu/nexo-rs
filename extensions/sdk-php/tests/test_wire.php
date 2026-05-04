<?php

declare(strict_types=1);

const ECHO_FIXTURE = __DIR__ . '/fixtures/echo-plugin.php';

function fail(string $msg): never
{
    fwrite(STDERR, "FAIL: $msg\n");
    exit(1);
}

// ── test 1: frame_over_max_size_logs_WireError_and_continues ──
// Build a frame larger than MAX_FRAME_BYTES (1 MiB). Filling
// the payload with 2 MiB of 'x'.
$huge = str_repeat('x', 2 * 1024 * 1024);
$oversize = json_encode([
    'jsonrpc' => '2.0',
    'id' => 1,
    'method' => 'broker.event',
    'params' => [
        'topic' => 'plugin.outbound.echo',
        'event' => ['topic' => 'x', 'source' => 'y', 'payload' => ['huge' => $huge]],
    ],
]) . "\n";
$normal = json_encode(['jsonrpc' => '2.0', 'id' => 2, 'method' => 'initialize']) . "\n";
$shutdown = json_encode(['jsonrpc' => '2.0', 'id' => 3, 'method' => 'shutdown']) . "\n";

$proc = proc_open(
    ['php', ECHO_FIXTURE],
    [0 => ['pipe', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']],
    $pipes,
);
if (!is_resource($proc)) {
    fail('proc_open failed');
}
fwrite($pipes[0], $oversize . $normal . $shutdown);
fclose($pipes[0]);
stream_set_blocking($pipes[1], false);
stream_set_blocking($pipes[2], false);
$stdout = '';
$stderr = '';
$deadline = microtime(true) + 15;
while (microtime(true) < $deadline) {
    $status = proc_get_status($proc);
    $stdout .= stream_get_contents($pipes[1]) ?: '';
    $stderr .= stream_get_contents($pipes[2]) ?: '';
    if (!$status['running']) {
        break;
    }
    usleep(50_000);
}
$stdout .= stream_get_contents($pipes[1]) ?: '';
$stderr .= stream_get_contents($pipes[2]) ?: '';
fclose($pipes[1]);
fclose($pipes[2]);
proc_close($proc);

// Oversized frame must NOT generate a JSON reply for id=1.
$hasIdOneResult = false;
foreach (explode("\n", $stdout) as $line) {
    $f = json_decode($line, true);
    if (is_array($f) && ($f['id'] ?? null) === 1 && isset($f['result'])) {
        $hasIdOneResult = true;
        break;
    }
}
if ($hasIdOneResult) {
    fail('frame_over_max_size: oversized frame must not be processed');
}
// Subsequent initialize must succeed.
$hasIdTwo = false;
foreach (explode("\n", $stdout) as $line) {
    $f = json_decode($line, true);
    if (is_array($f) && ($f['id'] ?? null) === 2) {
        $hasIdTwo = true;
        break;
    }
}
if (!$hasIdTwo) {
    fail('frame_over_max_size: initialize after oversize frame must still process. stdout=' . substr($stdout, 0, 500));
}
if (!str_contains($stderr, 'exceeds maxFrameBytes')) {
    fail('frame_over_max_size: expected wire error on stderr, got: ' . substr($stderr, 0, 500));
}
fwrite(STDOUT, "ok 1 - frame_over_max_size_logs_WireError_and_continues\n");

exit(0);
