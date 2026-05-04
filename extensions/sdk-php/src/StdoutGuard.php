<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — defensive guard on `echo` / `print` / `printf` /
 * `var_dump` writes that buffers + line-parses every output.
 *
 * Why: the daemon parses the plugin's stdout as newline-delimited
 * JSON-RPC frames. Any stray write — `echo "debug"` from the
 * plugin author's code, a chatty dependency banner, a `var_dump`
 * that slipped past review — would corrupt the parser mid-stream
 * with no recovery path. The guard converts those mistakes from
 * "fatal disconnect" into "tagged stderr line".
 *
 * IMPORTANT LIMITATION: `fwrite(STDOUT, ...)` direct writes
 * BYPASS this guard (PHP's `ob_start` only intercepts the
 * buffered output API). The SDK's own `BrokerSender::publish()`
 * uses direct `fwrite` deliberately so blessed frames always
 * reach the host. Plugin authors who need stdout output should
 * use `echo` / `print` / `printf` (guarded). Calling
 * `fwrite(STDOUT, ...)` directly from author code is undefined
 * behavior.
 */

namespace Nexo\Plugin\Sdk;

final class StdoutGuard
{
    public const MARKER = '[stdout-guard]';

    private static bool $installed = false;
    private static string $buffer = '';

    /**
     * Install the guard via `ob_start`. Idempotent.
     */
    public static function install(): void
    {
        if (self::$installed) {
            return;
        }
        self::$installed = true;
        self::$buffer = '';
        ob_start([self::class, 'callback'], 1);
    }

    /**
     * Uninstall (test harness only). Flushes any pending
     * non-newline-terminated buffer raw to STDOUT.
     */
    public static function uninstall(): void
    {
        if (!self::$installed) {
            return;
        }
        ob_end_clean();
        if (self::$buffer !== '') {
            fwrite(STDOUT, self::$buffer);
            self::$buffer = '';
        }
        self::$installed = false;
    }

    public static function isInstalled(): bool
    {
        return self::$installed;
    }

    /**
     * `ob_start` callback. Receives every buffered chunk and
     * decides what to forward / divert. Returns empty string so
     * the buffered content is discarded — we manage forwarding
     * ourselves via direct fwrite.
     *
     * @internal
     */
    public static function callback(string $chunk, int $phase): string
    {
        self::$buffer .= $chunk;
        while (($newlineIdx = strpos(self::$buffer, "\n")) !== false) {
            $line = substr(self::$buffer, 0, $newlineIdx);
            self::$buffer = substr(self::$buffer, $newlineIdx + 1);
            if (self::isJsonLine($line)) {
                fwrite(STDOUT, $line . "\n");
            } else {
                fwrite(STDERR, self::MARKER . ' ' . $line . "\n");
            }
        }
        // On flush phase, emit any remaining partial buffer raw
        // so debug output is not lost across uninstall/shutdown.
        if (($phase & PHP_OUTPUT_HANDLER_FINAL) !== 0 && self::$buffer !== '') {
            if (self::isJsonLine(self::$buffer)) {
                fwrite(STDOUT, self::$buffer);
            } else {
                fwrite(STDERR, self::MARKER . ' ' . self::$buffer . "\n");
            }
            self::$buffer = '';
        }
        return '';
    }

    private static function isJsonLine(string $line): bool
    {
        // Empty lines tolerated — trailing newlines and blank
        // separators inside an NDJSON stream do not corrupt
        // parsers.
        if ($line === '') {
            return true;
        }
        try {
            json_decode($line, true, 512, JSON_THROW_ON_ERROR);
            return true;
        } catch (\JsonException) {
            return false;
        }
    }
}
