/**
 * Phase 31.5 — wire-format hardening test: oversized frames must
 * be rejected with a WireError logged to stderr; the dispatch
 * loop continues processing subsequent frames.
 */

import { test } from "node:test";
import { strict as assert } from "node:assert";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const ECHO = join(HERE, "fixtures", "echo-plugin.mjs");

test("frame_over_max_size_logs_WireError_and_continues", async () => {
  const proc = spawn(process.execPath, [ECHO], { stdio: ["pipe", "pipe", "pipe"] });
  const captured = new Promise((resolve, reject) => {
    let so = "", se = "";
    proc.stdout.on("data", (d) => { so += d.toString("utf-8"); });
    proc.stderr.on("data", (d) => { se += d.toString("utf-8"); });
    proc.on("close", () => resolve({ stdout: so, stderr: se }));
    proc.on("error", reject);
  });

  // Build a frame larger than MAX_FRAME_BYTES (1 MiB). Filling
  // a single string field with 2 MiB of `x`s.
  const huge = "x".repeat(2 * 1024 * 1024);
  const oversize = JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "broker.event",
    params: { topic: "plugin.outbound.echo", event: { topic: "x", source: "y", payload: { huge } } },
  }) + "\n";
  proc.stdin.write(oversize);
  // Then a normal initialize+shutdown to prove dispatch continues.
  proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 2, method: "initialize" }) + "\n");
  proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 3, method: "shutdown" }) + "\n");
  proc.stdin.end();

  const { stdout, stderr } = await captured;
  // Oversize frame must NOT generate a JSON reply on stdout (no
  // id=1 result).
  const hasIdOneResult = stdout.split("\n").some((l) => {
    try { return JSON.parse(l).id === 1 && l.includes("result"); } catch { return false; }
  });
  assert.equal(hasIdOneResult, false, "oversized frame must not be processed");
  // Subsequent initialize must succeed.
  const init = stdout.split("\n").find((l) => {
    try { return JSON.parse(l).id === 2; } catch { return false; }
  });
  assert.ok(init, "initialize after oversize frame must still process");
  // stderr must mention exceeds maxFrameBytes.
  assert.ok(
    stderr.includes("exceeds maxFrameBytes"),
    `expected wire error on stderr, got: ${stderr}`,
  );
});
