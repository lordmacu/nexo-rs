<?php

declare(strict_types=1);

/**
 * Slow handler fixture: handler suspends a Fiber for ~200ms
 * before publishing its reply. Used to assert the reader does
 * not block on slow handlers + that in-flight Fibers are
 * drained on shutdown.
 */

require __DIR__ . '/../../vendor/autoload.php';

use Nexo\Plugin\Sdk\BrokerSender;
use Nexo\Plugin\Sdk\Event;
use Nexo\Plugin\Sdk\PluginAdapter;

$MANIFEST = <<<'TOML'
[plugin]
id = "slow_plugin"
version = "0.1.0"
name = "Slow"
description = "fixture"
min_nexo_version = ">=0.1.0"
TOML;

$adapter = new PluginAdapter([
    'manifestToml' => $MANIFEST,
    'handleProcessSignals' => false,
    'onEvent' => function (string $topic, Event $event, BrokerSender $broker): void {
        // Cooperative pause: yield ~200 times via Fiber::suspend()
        // with a 1ms wall sleep on each resume. Total wall time
        // ~200ms, but the reader (running in the main thread)
        // continues polling stdin between resumes.
        for ($i = 0; $i < 200; $i++) {
            usleep(1_000);
            \Fiber::suspend();
        }
        $out = Event::new('plugin.inbound.slow', 'slow_plugin', ['ack' => true]);
        $broker->publish('plugin.inbound.slow', $out);
    },
]);
$adapter->run();
