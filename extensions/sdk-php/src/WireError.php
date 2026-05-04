<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — raised on malformed JSON-RPC frames or
 * oversized lines (> `Wire::MAX_FRAME_BYTES`).
 */

namespace Nexo\Plugin\Sdk;

final class WireError extends PluginError
{
}
