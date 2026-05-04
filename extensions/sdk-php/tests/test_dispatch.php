<?php

declare(strict_types=1);

const ECHO_FIXTURE = __DIR__ . '/fixtures/echo-plugin.php';
const SLOW_FIXTURE = __DIR__ . '/fixtures/slow-plugin.php';

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

function jsonrpc_notification(string $method, array $params): string
{
    return json_encode(['jsonrpc' => '2.0', 'method' => $method, 'params' => $params]) . "\n";
}

function event_params(string $topic, array $payload): array
{
    return [
        'topic' => $topic,
        'event' => ['topic' => $topic, 'source' => 'host', 'payload' => $payload],
    ];
}

/**
 * @return array{stdout: string, stderr: string, code: int}
 */
function run_fixture(string $fixture, string $stdin, int $timeoutSec = 15): array
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

// ── test 1: broker_event_invokes_handler ───────────────────────
$res = run_fixture(
    ECHO_FIXTURE,
    jsonrpc_notification('broker.event', event_params('plugin.outbound.echo', ['hello' => 1]))
        . jsonrpc_request(99, 'shutdown'),
);
$lines = array_values(array_filter(explode("\n", $res['stdout']), fn($l) => trim($l) !== ''));
$publish = null;
foreach ($lines as $line) {
    $f = json_decode($line, true);
    if (($f['method'] ?? null) === 'broker.publish') {
        $publish = $f;
        break;
    }
}
if ($publish === null) {
    fail('broker_event_invokes_handler: publish line missing in ' . $res['stdout'] . ' stderr=' . $res['stderr']);
}
if (($publish['params']['topic'] ?? null) !== 'plugin.inbound.echoed') {
    fail('broker_event_invokes_handler: wrong topic ' . print_r($publish, true));
}
if (($publish['params']['event']['payload']['echoed'] ?? null) !== ['hello' => 1]) {
    fail('broker_event_invokes_handler: payload not echoed ' . print_r($publish, true));
}
fwrite(STDOUT, "ok 1 - broker_event_invokes_handler\n");

// ── test 2: handler_does_not_block_reader ──────────────────────
// Send broker.event for the slow handler (200ms) immediately
// followed by shutdown. Both must be processed; the reader
// must NOT block on the slow handler.
$res = run_fixture(
    SLOW_FIXTURE,
    jsonrpc_notification('broker.event', event_params('plugin.outbound.slow', ['x' => 1]))
        . jsonrpc_request(99, 'shutdown'),
);
$lines = array_values(array_filter(explode("\n", $res['stdout']), fn($l) => trim($l) !== ''));
$hasShutdown = false;
$hasPublish = false;
foreach ($lines as $line) {
    $f = json_decode($line, true);
    if (($f['id'] ?? null) === 99) {
        $hasShutdown = true;
    }
    if (($f['method'] ?? null) === 'broker.publish') {
        $hasPublish = true;
    }
}
if (!$hasShutdown) {
    fail('handler_does_not_block_reader: shutdown reply missing stdout=' . $res['stdout'] . ' stderr=' . $res['stderr']);
}
if (!$hasPublish) {
    fail('handler_does_not_block_reader: slow handler publish missing stdout=' . $res['stdout']);
}
fwrite(STDOUT, "ok 2 - handler_does_not_block_reader\n");

// ── test 3: inflight_fibers_drained_on_shutdown ────────────────
// Stronger: the slow handler's publish MUST appear despite
// shutdown being requested while the Fiber is still suspended.
$res = run_fixture(
    SLOW_FIXTURE,
    jsonrpc_notification('broker.event', event_params('plugin.outbound.slow', ['x' => 1]))
        . jsonrpc_request(99, 'shutdown'),
);
$publishLine = null;
foreach (explode("\n", $res['stdout']) as $line) {
    if (str_contains($line, 'broker.publish')) {
        $publishLine = $line;
        break;
    }
}
if ($publishLine === null) {
    fail('inflight_fibers_drained: slow handler publish must complete despite shutdown stdout=' . $res['stdout'] . ' stderr=' . $res['stderr']);
}
$frame = json_decode($publishLine, true);
if (($frame['params']['event']['payload']['ack'] ?? null) !== true) {
    fail('inflight_fibers_drained: payload missing ack ' . print_r($frame, true));
}
fwrite(STDOUT, "ok 3 - inflight_fibers_drained_on_shutdown\n");

exit(0);
