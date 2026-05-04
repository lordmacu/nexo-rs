/**
 * Phase 31.5 — child-side dispatch loop.
 *
 * Mirrors the Rust counterpart in
 * `crates/microapp-sdk/src/plugin.rs::PluginAdapter` and the
 * Python counterpart in `extensions/sdk-python/nexo_plugin_sdk/adapter.py`.
 *
 * Reads JSON-RPC 2.0 newline-delimited frames from stdin, dispatches:
 *
 * - `method == "initialize"` (request) → reply with manifest +
 *   server_version.
 * - `method == "broker.event"` (notification) → spawn a detached
 *   task running `onEvent` so the reader continues polling stdin
 *   while the handler awaits its own broker round-trips.
 * - `method == "shutdown"` (request) → drain in-flight tasks,
 *   reply `{ok: true}`, exit the loop.
 * - Anything else with an id → reply error `-32601 method not found`.
 * - Anything else without an id (notification) → silently ignore
 *   (JSON-RPC 2.0 §4.1).
 */

import { Buffer } from "node:buffer";
import * as readline from "node:readline";

import { BrokerSender } from "./broker.js";
import { ManifestError, PluginError, WireError } from "./errors.js";
import { Event } from "./events.js";
import { parseManifest, type ParsedManifest } from "./manifest.js";
import {
  installStdoutGuard,
  isStdoutGuardInstalled,
} from "./stdout-guard.js";
import {
  buildErrorResponse,
  buildResponse,
  JSONRPC_VERSION,
  MAX_FRAME_BYTES,
  serializeFrame,
} from "./wire.js";

export type EventHandler = (
  topic: string,
  event: Event,
  broker: BrokerSender,
) => Promise<void>;

export type ShutdownHandler = () => Promise<void>;

export interface PluginAdapterOptions {
  /** Body of nexo-plugin.toml. Parsed once at construction. */
  manifestToml: string;
  /** Returned in the initialize reply. Default `"0.1.0"`. */
  serverVersion?: string;
  /** Invoked for every broker.event notification. Detached task —
   * the reader does not block while the handler awaits its own
   * broker.publish round-trips. */
  onEvent?: EventHandler;
  /** Awaited before `{ok: true}` reply to shutdown. */
  onShutdown?: ShutdownHandler;
  /** Default true — patches `process.stdout.write` to divert
   * non-JSON lines to stderr. Critical for plugin authors who
   * accidentally `console.log`. Set false only if you have
   * another guard layer. */
  enableStdoutGuard?: boolean;
  /** Default `MAX_FRAME_BYTES` (1 MiB). Reject inbound frames
   * larger than this with a WireError; dispatch continues. */
  maxFrameBytes?: number;
  /** Default true — listen for SIGTERM + SIGINT and trigger
   * graceful shutdown (drain in-flight, exit 0). */
  handleProcessSignals?: boolean;
}

interface JsonRpcFrameLike {
  jsonrpc?: unknown;
  id?: unknown;
  method?: unknown;
  params?: unknown;
}

export class PluginAdapter {
  private readonly parsed: ParsedManifest;
  private readonly serverVersion: string;
  private readonly onEvent?: EventHandler;
  private readonly onShutdown?: ShutdownHandler;
  private readonly maxFrameBytes: number;
  private readonly handleProcessSignals: boolean;
  private readonly inflight = new Set<Promise<void>>();
  private readonly broker: BrokerSender;

  private started = false;
  private stopped = false;
  private rl: readline.Interface | null = null;
  private signalCleanup: (() => void) | null = null;

  constructor(opts: PluginAdapterOptions) {
    this.parsed = parseManifest(opts.manifestToml);
    this.serverVersion = opts.serverVersion ?? "0.1.0";
    this.onEvent = opts.onEvent;
    this.onShutdown = opts.onShutdown;
    this.maxFrameBytes = opts.maxFrameBytes ?? MAX_FRAME_BYTES;
    this.handleProcessSignals = opts.handleProcessSignals ?? true;

    if (opts.enableStdoutGuard !== false) {
      installStdoutGuard();
    }

    this.broker = new BrokerSender((line) => {
      // Direct stdout write through the original handle — the
      // guard would no-op on JSON lines anyway, but skipping it
      // saves one parse round-trip per publish. The guard remains
      // installed for everything OTHER than the SDK's blessed
      // path (e.g. console.log from author code).
      process.stdout.write(line);
    });
  }

  get manifest(): Readonly<Record<string, unknown>> {
    return this.parsed.raw;
  }

  /** Single-shot. Throws PluginError if called twice. */
  async run(): Promise<void> {
    if (this.started) {
      throw new PluginError("PluginAdapter.run() already invoked");
    }
    this.started = true;

    if (this.handleProcessSignals) {
      const onSig = (): void => {
        // Closing the readline interface causes the for-await loop
        // below to break; the surrounding code handles in-flight
        // drain and exit.
        this.rl?.close();
      };
      process.on("SIGTERM", onSig);
      process.on("SIGINT", onSig);
      this.signalCleanup = (): void => {
        process.removeListener("SIGTERM", onSig);
        process.removeListener("SIGINT", onSig);
      };
    }

    this.rl = readline.createInterface({
      input: process.stdin,
      terminal: false,
      crlfDelay: Infinity,
    });

    try {
      for await (const rawLine of this.rl) {
        if (this.stopped) {
          break;
        }
        await this.handleLine(rawLine);
      }
    } finally {
      this.rl?.close();
      this.rl = null;
      this.signalCleanup?.();
      this.signalCleanup = null;
      // Drain any handlers spawned mid-stream that haven't replied
      // yet — most relevant for SIGTERM-initiated exits where
      // shutdown was not received.
      await this.drainInflight();
    }
  }

  private async handleLine(line: string): Promise<void> {
    if (line.length === 0) {
      return;
    }
    const byteLen = Buffer.byteLength(line, "utf-8");
    if (byteLen > this.maxFrameBytes) {
      const err = new WireError(
        `inbound frame ${byteLen} bytes exceeds maxFrameBytes ${this.maxFrameBytes}`,
      );
      process.stderr.write(`plugin: ${err.message}\n`);
      return;
    }

    let msg: JsonRpcFrameLike;
    try {
      msg = JSON.parse(line);
    } catch (e) {
      const reason = e instanceof Error ? e.message : String(e);
      process.stderr.write(`plugin: malformed jsonrpc line: ${reason}\n`);
      return;
    }
    if (typeof msg !== "object" || msg === null) {
      process.stderr.write(`plugin: jsonrpc frame must be an object\n`);
      return;
    }

    const method = msg.method;
    const id = msg.id;
    if (typeof method !== "string") {
      // No method → spurious response; ignore.
      return;
    }

    if (method === "initialize") {
      this.replyInitialize(id);
    } else if (method === "broker.event") {
      this.dispatchEvent(msg.params);
    } else if (method === "shutdown") {
      await this.replyShutdown(id);
      this.stopped = true;
      this.rl?.close();
    } else if (id !== undefined) {
      // Unknown request — JSON-RPC requires a reply.
      this.writeFrame(buildErrorResponse(id as never, -32601, "method not found"));
    }
    // Unknown notification (no id) — silently ignore per JSON-RPC §4.1.
  }

  private replyInitialize(id: unknown): void {
    if (id === undefined) {
      // No id → spec-violating notification; nothing to reply to.
      return;
    }
    this.writeFrame(
      buildResponse(id as never, {
        manifest: this.parsed.raw,
        server_version: this.serverVersion,
      }),
    );
  }

  private async replyShutdown(id: unknown): Promise<void> {
    await this.drainInflight();
    if (this.onShutdown !== undefined) {
      try {
        await this.onShutdown();
      } catch (e) {
        const reason = e instanceof Error ? e.message : String(e);
        process.stderr.write(`plugin: onShutdown raised: ${reason}\n`);
      }
    }
    if (id !== undefined) {
      this.writeFrame(buildResponse(id as never, { ok: true }));
    }
  }

  private dispatchEvent(params: unknown): void {
    if (this.onEvent === undefined) {
      return;
    }
    let topic: string;
    let event: Event;
    try {
      if (typeof params !== "object" || params === null) {
        throw new WireError("broker.event params must be a JSON object");
      }
      const p = params as { topic?: unknown; event?: unknown };
      if (typeof p.topic !== "string") {
        throw new WireError("broker.event params missing string `topic`");
      }
      topic = p.topic;
      event = Event.fromJson(p.event);
    } catch (e) {
      const reason = e instanceof Error ? e.message : String(e);
      process.stderr.write(`plugin: dispatch decode failed: ${reason}\n`);
      return;
    }

    const handler = this.onEvent;
    const task: Promise<void> = Promise.resolve()
      .then(() => handler(topic, event, this.broker))
      .catch((e) => {
        const reason = e instanceof Error ? e.message : String(e);
        process.stderr.write(`plugin: onEvent raised: ${reason}\n`);
      });
    this.inflight.add(task);
    task.finally(() => {
      this.inflight.delete(task);
    });
  }

  private writeFrame(frame: Parameters<typeof serializeFrame>[0]): void {
    process.stdout.write(serializeFrame(frame));
  }

  private async drainInflight(): Promise<void> {
    if (this.inflight.size === 0) {
      return;
    }
    await Promise.allSettled([...this.inflight]);
  }
}

// Re-export ManifestError so importers can tell it apart from
// generic PluginError without pulling errors.js too.
export { ManifestError } from "./errors.js";

// Touch isStdoutGuardInstalled so test suites can assert the
// constructor's side effect; not exported as part of the public
// API surface.
void isStdoutGuardInstalled;
