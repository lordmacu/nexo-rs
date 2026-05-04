/**
 * Phase 31.5 — child-side broker handle.
 *
 * Plugin authors call `broker.publish(topic, event)` to emit
 * notifications back to the daemon. Topics MUST appear on the
 * manifest's `[[plugin.channels.register]]` allowlist; the host
 * drops disallowed topics with a warn log (defense in depth).
 *
 * Concurrency: a Promise-chain write lock serializes outbound
 * frames so concurrent handler tasks never interleave half-written
 * JSON lines on stdout. Callers `await broker.publish(...)`; the
 * returned Promise resolves once the byte sequence has been flushed
 * to the parent's stdin.
 */

import { Event } from "./events.js";
import { JSONRPC_VERSION, serializeFrame } from "./wire.js";

export type LineWriter = (line: string) => void;

export class BrokerSender {
  /** Bounded by the JS event loop's microtask queue — adds zero
   * scheduling overhead per call. Each `publish` chains
   * `writeChain = writeChain.then(doWrite)`, so the second caller
   * waits until the first completes synchronously inside the
   * thenable, preserving FIFO order without async semaphores.
   */
  private writeChain: Promise<void> = Promise.resolve();
  private readonly write: LineWriter;

  constructor(write: LineWriter) {
    this.write = write;
  }

  async publish(topic: string, event: Event): Promise<void> {
    const next = this.writeChain.then(() => this.doWrite(topic, event));
    this.writeChain = next.catch(() => {
      // Surface the error to the awaiting caller via the next
      // assignment, but reset the chain so a single failed write
      // does not poison every subsequent publish.
    });
    return next;
  }

  private doWrite(topic: string, event: Event): void {
    const frame = {
      jsonrpc: JSONRPC_VERSION as typeof JSONRPC_VERSION,
      method: "broker.publish",
      params: { topic, event: Event.toJson(event) },
    };
    this.write(serializeFrame(frame));
  }
}
