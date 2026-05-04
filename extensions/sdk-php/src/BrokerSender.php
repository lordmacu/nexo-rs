<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — child-side broker handle.
 *
 * Plugin authors call `$broker->publish($topic, $event)` to emit
 * notifications back to the daemon. Topics MUST appear on the
 * manifest's `[[plugin.channels.register]]` allowlist; the host
 * drops disallowed topics with a warn log (defense in depth).
 *
 * Concurrency: PHP is single-threaded and Fibers do NOT preempt
 * mid-call, so sequential `fwrite(STDOUT, ...)` calls preserve
 * FIFO ordering without explicit locking. The direct fwrite
 * also bypasses `ob_start` (the StdoutGuard) so blessed JSON
 * frames always reach the host even when the guard is active.
 */

namespace Nexo\Plugin\Sdk;

final class BrokerSender
{
    public function publish(string $topic, Event $event): void
    {
        $frame = [
            'jsonrpc' => Wire::JSONRPC_VERSION,
            'method' => 'broker.publish',
            'params' => [
                'topic' => $topic,
                'event' => $event->toJson(),
            ],
        ];
        fwrite(STDOUT, Wire::serializeFrame($frame));
        fflush(STDOUT);
    }
}
