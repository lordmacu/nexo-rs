/**
 * Phase 31.5 — defensive guard on `process.stdout.write` that
 * intercepts every write, line-buffers, and only forwards lines
 * that successfully `JSON.parse`. Non-JSON lines are diverted to
 * stderr tagged with the `STDOUT_GUARD_MARKER` sentinel.
 *
 * Why: the daemon parses the plugin's stdout as newline-delimited
 * JSON-RPC frames. Any stray write — `console.log("hello")` from
 * the plugin author's code, a chatty dependency banner, a debug
 * print that slipped past review — would corrupt the parser
 * mid-stream with no recovery path. The guard converts those
 * mistakes from "fatal disconnect" into "tagged stderr line".
 *
 * The blessed write path (BrokerSender / PluginAdapter)
 * always emits valid JSON, so its frames pass through
 * unchanged.
 */

import { Buffer } from "node:buffer";

export const STDOUT_GUARD_MARKER = "[stdout-guard]";

let installed = false;
let buffer = "";
type StdoutWrite = typeof process.stdout.write;
let originalWrite: StdoutWrite | null = null;

function isJsonLine(line: string): boolean {
  // Empty lines tolerated — trailing newlines and blank
  // separators inside an NDJSON stream do not corrupt parsers.
  if (line.length === 0) {
    return true;
  }
  try {
    JSON.parse(line);
    return true;
  } catch {
    return false;
  }
}

function chunkToString(
  chunk: string | Buffer | Uint8Array,
  encoding?: BufferEncoding,
): string {
  if (typeof chunk === "string") {
    return chunk;
  }
  if (chunk instanceof Buffer) {
    return chunk.toString(encoding ?? "utf-8");
  }
  return Buffer.from(chunk).toString(encoding ?? "utf-8");
}

export function installStdoutGuard(): void {
  if (installed) {
    return;
  }
  installed = true;
  originalWrite = process.stdout.write.bind(process.stdout) as StdoutWrite;

  const wrapped: StdoutWrite = ((
    chunk: string | Buffer | Uint8Array,
    encodingOrCb?: BufferEncoding | ((err?: Error | null) => void),
    cb?: (err?: Error | null) => void,
  ): boolean => {
    let encoding: BufferEncoding | undefined;
    let callback: ((err?: Error | null) => void) | undefined;
    if (typeof encodingOrCb === "function") {
      callback = encodingOrCb;
    } else if (typeof encodingOrCb === "string") {
      encoding = encodingOrCb;
      callback = cb;
    }
    void callback; // not invoked by guard; original handles it.

    const text = chunkToString(chunk, encoding);
    buffer += text;
    let wrote = true;
    let newlineIdx: number;
    while ((newlineIdx = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, newlineIdx);
      buffer = buffer.slice(newlineIdx + 1);
      if (isJsonLine(line)) {
        wrote = originalWrite!(line + "\n");
      } else {
        process.stderr.write(`${STDOUT_GUARD_MARKER} ${line}\n`);
      }
    }
    return wrote;
  }) as StdoutWrite;

  process.stdout.write = wrapped;
}

export function uninstallStdoutGuard(): void {
  if (!installed || originalWrite === null) {
    return;
  }
  // Flush any buffered partial line (no trailing newline) — emit
  // it raw to stdout so debug output isn't lost across uninstall.
  if (buffer.length > 0) {
    originalWrite(buffer);
    buffer = "";
  }
  process.stdout.write = originalWrite;
  originalWrite = null;
  installed = false;
}

export function isStdoutGuardInstalled(): boolean {
  return installed;
}
