<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — Event payload mirroring the Rust SDK's `Event`
 * shape and the host's broker event shape.
 */

namespace Nexo\Plugin\Sdk;

final class Event
{
    /**
     * @param array<string, mixed> $payload
     * @param array<string, mixed> $metadata
     */
    public function __construct(
        public readonly string $topic,
        public readonly string $source,
        public readonly array $payload,
        public readonly ?string $correlationId = null,
        public readonly array $metadata = [],
    ) {
    }

    /**
     * Build a fresh event. The most common constructor — handler
     * code uses this when echoing payloads back to the broker.
     *
     * @param array<string, mixed> $payload
     */
    public static function new(string $topic, string $source, array $payload): self
    {
        return new self($topic, $source, $payload);
    }

    /**
     * Round-trip a JSON-RPC `event` field into a typed Event.
     * Validates `topic` and `source` are non-empty strings;
     * throws WireError on shape mismatch so the dispatch loop
     * can log + skip the frame.
     *
     * @param array<string, mixed> $data
     */
    public static function fromJson(array $data): self
    {
        $topic = $data['topic'] ?? null;
        if (!is_string($topic) || $topic === '') {
            throw new WireError('event.topic missing or not a non-empty string');
        }
        $source = $data['source'] ?? null;
        if (!is_string($source) || $source === '') {
            throw new WireError('event.source missing or not a non-empty string');
        }
        $payload = $data['payload'] ?? [];
        if (!is_array($payload)) {
            $payload = [];
        }
        $correlationId = null;
        if (isset($data['correlation_id']) && is_string($data['correlation_id'])) {
            $correlationId = $data['correlation_id'];
        }
        $metadata = [];
        if (isset($data['metadata']) && is_array($data['metadata'])) {
            $metadata = $data['metadata'];
        }
        return new self($topic, $source, $payload, $correlationId, $metadata);
    }

    /**
     * Serialize to the wire format. Omits absent optional fields.
     *
     * @return array<string, mixed>
     */
    public function toJson(): array
    {
        $out = [
            'topic' => $this->topic,
            'source' => $this->source,
            'payload' => $this->payload,
        ];
        if ($this->correlationId !== null) {
            $out['correlation_id'] = $this->correlationId;
        }
        if ($this->metadata !== []) {
            $out['metadata'] = $this->metadata;
        }
        return $out;
    }
}
