/**
 * Phase 31.5 — Event payload mirroring the Rust SDK's `Event`
 * shape and the host's broker event shape.
 */

import { WireError } from "./errors.js";

export interface Event {
  topic: string;
  source: string;
  payload: Record<string, unknown>;
  correlation_id?: string;
  metadata?: Record<string, unknown>;
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

export const Event = {
  /** Build a fresh event. The most common constructor — handler
   * code uses this when echoing payloads back to the broker.
   */
  new(topic: string, source: string, payload: Record<string, unknown>): Event {
    return { topic, source, payload };
  },

  /** Round-trip a JSON-RPC `event` field into a typed Event.
   * Validates `topic` and `source` are strings; throws WireError
   * on shape mismatch so dispatch code can log + skip the frame.
   */
  fromJson(d: unknown): Event {
    if (!isPlainObject(d)) {
      throw new WireError("event must be a JSON object");
    }
    const topic = d["topic"];
    if (typeof topic !== "string" || topic.length === 0) {
      throw new WireError("event.topic missing or not a string");
    }
    const source = d["source"];
    if (typeof source !== "string" || source.length === 0) {
      throw new WireError("event.source missing or not a string");
    }
    const payloadRaw = d["payload"];
    const payload: Record<string, unknown> = isPlainObject(payloadRaw)
      ? payloadRaw
      : {};
    const event: Event = { topic, source, payload };
    const corr = d["correlation_id"];
    if (typeof corr === "string") {
      event.correlation_id = corr;
    }
    const meta = d["metadata"];
    if (isPlainObject(meta)) {
      event.metadata = meta;
    }
    return event;
  },

  /** Serialize to the wire format (omits absent optional fields). */
  toJson(e: Event): Record<string, unknown> {
    const out: Record<string, unknown> = {
      topic: e.topic,
      source: e.source,
      payload: e.payload,
    };
    if (e.correlation_id !== undefined) {
      out["correlation_id"] = e.correlation_id;
    }
    if (e.metadata !== undefined && Object.keys(e.metadata).length > 0) {
      out["metadata"] = e.metadata;
    }
    return out;
  },
} as const;
