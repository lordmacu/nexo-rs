<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — handshake tests for the PHP plugin SDK.
 *
 * Black-box exercise: spawn a child PHP process running the
 * echo fixture, send JSON-RPC frames over stdin, assert lines
 * on stdout. The wire format is the source of truth so we do
 * not touch SDK internals.
 */

const FIXTURE = __DIR__ . '/fixtures/echo-plugin.php';

function fail(string $msg): never
{
    fwrite(STDERR, "FAIL: $msg\n");
    exit(1);
}

function jsonrpc_request(int $id, string $method, ?array $params = null): string
{
    $frame = ['jsonrpc' => '2.0', 'id' => $id, 'method' => $method];
    if ($params !== null) {
        $frame['params'] = $params;
    }
    return json_encode($frame) . "\n";
}

/**
 * @return array{stdout: string, stderr: string, code: int}
 */
function run_fixture(string $fixture, string $stdin, int $timeoutSec = 10): array
{
    $proc = proc_open(
        ['php', $fixture],
        [
            0 => ['pipe', 'r'],
            1 => ['pipe', 'w'],
            2 => ['pipe', 'w'],
        ],
        $pipes,
    );
    if (!is_resource($proc)) {
        fail('proc_open failed');
    }
    fwrite($pipes[0], $stdin);
    fclose($pipes[0]);
    stream_set_blocking($pipes[1], false);
    stream_set_blocking($pipes[2], false);
    $stdout = '';
    $stderr = '';
    $deadline = microtime(true) + $timeoutSec;
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
    return ['stdout' => $stdout, 'stderr' => $stderr, 'code' => $code];
}

// ── test 1: initialize_returns_manifest ──────────────────────────
$res = run_fixture(FIXTURE, jsonrpc_request(1, 'initialize') . jsonrpc_request(2, 'shutdown'));
$lines = array_values(array_filter(explode("\n", $res['stdout']), fn($l) => trim($l) !== ''));
if (count($lines) < 2) {
    fail('initialize_returns_manifest: expected ≥2 reply lines, got ' . count($lines) . ' stdout=' . $res['stdout'] . ' stderr=' . $res['stderr']);
}
$first = json_decode($lines[0], true);
if ($first['jsonrpc'] !== '2.0' || $first['id'] !== 1) {
    fail('initialize_returns_manifest: bad jsonrpc/id ' . print_r($first, true));
}
if (($first['result']['server_version'] ?? null) !== '0.0.99') {
    fail('initialize_returns_manifest: bad server_version ' . print_r($first, true));
}
if (($first['result']['manifest']['plugin']['id'] ?? null) !== 'echo_plugin') {
    fail('initialize_returns_manifest: bad manifest.plugin.id ' . print_r($first, true));
}
$second = json_decode($lines[1], true);
if ($second['id'] !== 2 || ($second['result']['ok'] ?? null) !== true) {
    fail('initialize_returns_manifest: bad shutdown reply ' . print_r($second, true));
}
fwrite(STDOUT, "ok 1 - initialize_returns_manifest\n");

// ── test 2: unknown_method_returns_error_minus_32601 ────────────
$res = run_fixture(FIXTURE, jsonrpc_request(7, 'garbage.method') . jsonrpc_request(8, 'shutdown'));
$lines = array_values(array_filter(explode("\n", $res['stdout']), fn($l) => trim($l) !== ''));
if (count($lines) < 2) {
    fail('unknown_method: expected ≥2 lines stdout=' . $res['stdout']);
}
$first = json_decode($lines[0], true);
if ($first['id'] !== 7 || !isset($first['error']) || $first['error']['code'] !== -32601) {
    fail('unknown_method: bad error reply ' . print_r($first, true));
}
fwrite(STDOUT, "ok 2 - unknown_method_returns_error_minus_32601\n");

// ── test 3: unknown_notification_silently_ignored ──────────────
$notif = json_encode(['jsonrpc' => '2.0', 'method' => 'garbage.notif']) . "\n";
$res = run_fixture(FIXTURE, $notif . jsonrpc_request(9, 'shutdown'));
$lines = array_values(array_filter(explode("\n", $res['stdout']), fn($l) => trim($l) !== ''));
if (count($lines) !== 1) {
    fail('unknown_notification: expected exactly 1 reply (shutdown only), got ' . count($lines) . ' stdout=' . $res['stdout']);
}
$only = json_decode($lines[0], true);
if ($only['id'] !== 9) {
    fail('unknown_notification: expected shutdown reply id=9, got ' . print_r($only, true));
}
fwrite(STDOUT, "ok 3 - unknown_notification_silently_ignored\n");

exit(0);
