<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — JSON-RPC 2.0 frame helpers + wire constants.
 *
 * Frames are serialized as one JSON object per line, terminated
 * by `\n`. The host's reader uses readline-style line buffering;
 * the child does the same via blocking `fgets(STDIN)` polled
 * with `stream_select`.
 */

namespace Nexo\Plugin\Sdk;

final class Wire
{
    public const JSONRPC_VERSION = '2.0';

    /**
     * Maximum byte length of a single inbound JSON-RPC frame the
     * SDK accepts. Frames larger than this are rejected with a
     * WireError so an adversarial host cannot OOM the plugin via
     * a single huge line.
     */
    public const MAX_FRAME_BYTES = 1048576; // 1 MiB

    /**
     * Build a typed response.
     *
     * @param array<string, mixed> $result
     * @return array<string, mixed>
     */
    public static function buildResponse(int|string|null $id, array $result): array
    {
        return [
            'jsonrpc' => self::JSONRPC_VERSION,
            'id' => $id,
            'result' => $result,
        ];
    }

    /**
     * Build an error response.
     *
     * @return array<string, mixed>
     */
    public static function buildErrorResponse(int|string|null $id, int $code, string $message): array
    {
        return [
            'jsonrpc' => self::JSONRPC_VERSION,
            'id' => $id,
            'error' => ['code' => $code, 'message' => $message],
        ];
    }

    /**
     * Serialize a frame to a single line terminated by `\n`.
     *
     * @param array<string, mixed> $frame
     */
    public static function serializeFrame(array $frame): string
    {
        return json_encode($frame, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE) . "\n";
    }
}
