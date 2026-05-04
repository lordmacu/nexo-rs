<?php

declare(strict_types=1);

/**
 * Echo fixture for dispatch tests. Reads MANIFEST + behavior
 * flags from env so the same fixture serves multiple test cases.
 */

require __DIR__ . '/../../vendor/autoload.php';

use Nexo\Plugin\Sdk\BrokerSender;
use Nexo\Plugin\Sdk\Event;
use Nexo\Plugin\Sdk\PluginAdapter;

$DEFAULT_MANIFEST = <<<'TOML'
[plugin]
id = "echo_plugin"
version = "0.1.0"
name = "Echo"
description = "fixture"
min_nexo_version = ">=0.1.0"
TOML;

$adapter = new PluginAdapter([
    'manifestToml' => getenv('FIXTURE_MANIFEST') ?: $DEFAULT_MANIFEST,
    'serverVersion' => getenv('FIXTURE_SERVER_VERSION') ?: '0.0.99',
    'enableStdoutGuard' => getenv('FIXTURE_DISABLE_GUARD') !== '1',
    'handleProcessSignals' => false,
    'onEvent' => function (string $topic, Event $event, BrokerSender $broker): void {
        $out = Event::new(
            'plugin.inbound.echoed',
            'echo_plugin',
            [
                'echoed' => $event->payload,
                'incoming_topic' => $topic,
            ],
        );
        $broker->publish('plugin.inbound.echoed', $out);
    },
    'onShutdown' => function (): void {
        fwrite(STDERR, "echo_plugin shutdown_handler invoked\n");
    },
]);
$adapter->run();
