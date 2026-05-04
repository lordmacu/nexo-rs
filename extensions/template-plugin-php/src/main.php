<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — PHP plugin template entrypoint.
 *
 * Mirrors the structure of
 * `extensions/template-plugin-typescript/src/main.ts` and
 * `extensions/template-plugin-python/src/main.py`: parse the
 * bundled manifest, build a PluginAdapter, register an echo
 * handler, drive the dispatch loop until the daemon sends
 * `shutdown`.
 *
 * Replace `onEvent` with your own channel logic. Topics on
 * the allowlist for this plugin (per the manifest's
 * `[[plugin.channels.register]]` entry) are
 * `plugin.outbound.template_echo_php[.<instance>]` for inbound
 * events from the daemon and
 * `plugin.inbound.template_echo_php[.<instance>]` for messages
 * you send back through `$broker->publish(...)`.
 */

require __DIR__ . '/../vendor/autoload.php';

use Nexo\Plugin\Sdk\BrokerSender;
use Nexo\Plugin\Sdk\Event;
use Nexo\Plugin\Sdk\PluginAdapter;

// When run from a packed tarball, `bin/<id>` exec's
// `php lib/plugin/main.php`. The manifest sits at
// `<plugin_dir>/nexo-plugin.toml`; `__DIR__` is `lib/plugin/`,
// so we walk up two parents to reach it. In dev (`php
// src/main.php`) the manifest is at `../nexo-plugin.toml`.
$manifestPath = __DIR__ . '/../../nexo-plugin.toml';
if (!is_file($manifestPath)) {
    $manifestPath = __DIR__ . '/../nexo-plugin.toml';
}
$manifestToml = file_get_contents($manifestPath);
if ($manifestToml === false) {
    fwrite(STDERR, "plugin: failed to read manifest at $manifestPath\n");
    exit(1);
}

$adapter = new PluginAdapter([
    'manifestToml' => $manifestToml,
    'serverVersion' => '0.1.0',
    'onEvent' => function (string $topic, Event $event, BrokerSender $broker): void {
        $outTopic = str_starts_with($topic, 'plugin.outbound.')
            ? 'plugin.inbound.' . substr($topic, strlen('plugin.outbound.'))
            : 'plugin.inbound.' . $topic;
        $out = Event::new(
            $outTopic,
            $event->source !== '' ? $event->source : 'template_plugin_php',
            ['echoed' => $event->payload, 'incoming_topic' => $topic],
        );
        try {
            $broker->publish($outTopic, $out);
        } catch (\Throwable $e) {
            fwrite(STDERR, 'plugin: publish failed: ' . $e->getMessage() . "\n");
        }
    },
    'onShutdown' => function (): void {
        fwrite(STDERR, "template_plugin_php: shutdown requested\n");
    },
]);

$adapter->run();
