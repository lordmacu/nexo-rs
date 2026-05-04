<?php

declare(strict_types=1);

require __DIR__ . '/../vendor/autoload.php';

use Nexo\Plugin\Sdk\Event;
use Nexo\Plugin\Sdk\WireError;

function fail(string $msg): never
{
    fwrite(STDERR, "FAIL: $msg\n");
    exit(1);
}

// ── test 1: from_json_validates_required_fields ────────────────
try {
    Event::fromJson([]);
    fail('from_json: expected WireError on empty array');
} catch (WireError) {
}

try {
    Event::fromJson(['topic' => 'x']);
    fail('from_json: expected WireError when source missing');
} catch (WireError) {
}

// Happy path round-trip.
$ev = Event::fromJson([
    'topic' => 't',
    'source' => 's',
    'payload' => ['k' => 'v'],
    'correlation_id' => 'c1',
    'metadata' => ['m' => 1],
]);
if ($ev->topic !== 't' || $ev->source !== 's' || $ev->payload !== ['k' => 'v']) {
    fail('from_json: round-trip mismatch ' . print_r($ev, true));
}
if ($ev->correlationId !== 'c1') {
    fail('from_json: correlation_id not preserved');
}
$json = $ev->toJson();
if (($json['correlation_id'] ?? null) !== 'c1' || ($json['metadata'] ?? null) !== ['m' => 1]) {
    fail('to_json: optional fields missing ' . print_r($json, true));
}

// Optional fields omitted in toJson() when absent.
$bare = Event::new('t', 's', []);
$bareJson = $bare->toJson();
if (array_key_exists('correlation_id', $bareJson) || array_key_exists('metadata', $bareJson)) {
    fail('to_json: bare event must omit optional fields');
}

fwrite(STDOUT, "ok 1 - from_json_validates_required_fields\n");

exit(0);
