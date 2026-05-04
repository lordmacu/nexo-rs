/**
 * Phase 31.5 — broker.event dispatch tests.
 */

import { test } from "node:test";
import { strict as assert } from "node:assert";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const ECHO = join(HERE, "fixtures", "echo-plugin.mjs");
const SLOW = join(HERE, "fixtures", "slow-plugin.mjs");

function jsonrpcNotification(method, params) {
  return JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n";
}

function jsonrpcRequest(id, method, params) {
  const frame = { jsonrpc: "2.0", id, method };
  if (params !== undefined) frame.params = params;
  return JSON.stringify(frame) + "\n";
}

function eventParams(topic, payload) {
  return {
    topic,
    event: { topic, source: "host", payload },
  };
}

function readStdout(proc) {
  return new Promise((resolve, reject) => {
    let buf = "";
    let stderrBuf = "";
    proc.stdout.on("data", (d) => { buf += d.toString("utf-8"); });
    proc.stderr.on("data", (d) => { stderrBuf += d.toString("utf-8"); });
    proc.on("close", () => resolve({ stdout: buf, stderr: stderrBuf }));
    proc.on("error", reject);
  });
}

test("broker_event_invokes_handler", async () => {
  const proc = spawn(process.execPath, [ECHO], { stdio: ["pipe", "pipe", "pipe"] });
  const captured = readStdout(proc);
  proc.stdin.write(jsonrpcNotification("broker.event", eventParams("plugin.outbound.echo", { hello: 1 })));
  proc.stdin.write(jsonrpcRequest(99, "shutdown"));
  proc.stdin.end();

  const { stdout } = await captured;
  const lines = stdout.split("\n").filter((l) => l.trim().length > 0);
  // First line is the broker.publish notification from the
  // handler; second is the shutdown reply.
  const publish = lines.find((l) => {
    try { return JSON.parse(l).method === "broker.publish"; }
    catch { return false; }
  });
  assert.ok(publish, `publish line missing: ${JSON.stringify(lines)}`);
  const frame = JSON.parse(publish);
  assert.equal(frame.params.topic, "plugin.inbound.echoed");
  assert.deepEqual(frame.params.event.payload.echoed, { hello: 1 });
  assert.equal(frame.params.event.payload.incoming_topic, "plugin.outbound.echo");
});

test("handler_does_not_block_reader", async () => {
  // The slow handler awaits 200ms before publishing. Sending
  // shutdown immediately after the broker.event must still be
  // received by the reader (i.e. the reader is NOT blocked on
  // the slow handler's awaits).
  const proc = spawn(process.execPath, [SLOW], { stdio: ["pipe", "pipe", "pipe"] });
  const captured = readStdout(proc);
  proc.stdin.write(jsonrpcNotification("broker.event", eventParams("plugin.outbound.slow", { x: 1 })));
  proc.stdin.write(jsonrpcRequest(99, "shutdown"));
  proc.stdin.end();

  const { stdout } = await captured;
  const lines = stdout.split("\n").filter((l) => l.trim().length > 0);
  const ids = lines.map((l) => {
    try {
      const f = JSON.parse(l);
      return f.id ?? f.method;
    } catch { return null; }
  });
  // Both publish (method) and shutdown_reply (id=99) must arrive;
  // ordering depends on asyncio-style scheduling but both must
  // exist.
  assert.ok(ids.includes(99), `shutdown reply missing: ${JSON.stringify(lines)}`);
  assert.ok(
    lines.some((l) => l.includes("broker.publish")),
    `slow handler publish missing: ${JSON.stringify(lines)}`,
  );
});

test("inflight_handlers_drained_on_shutdown", async () => {
  // Stronger version of the previous test: the slow handler's
  // publish MUST appear (no mid-publish cancellation), even
  // though shutdown is requested while it's still running.
  const proc = spawn(process.execPath, [SLOW], { stdio: ["pipe", "pipe", "pipe"] });
  const captured = readStdout(proc);
  proc.stdin.write(jsonrpcNotification("broker.event", eventParams("plugin.outbound.slow", { x: 1 })));
  // Send shutdown 50ms later so the handler is mid-await.
  await new Promise((r) => setTimeout(r, 50));
  proc.stdin.write(jsonrpcRequest(99, "shutdown"));
  proc.stdin.end();

  const { stdout } = await captured;
  const publishLine = stdout.split("\n").find((l) => l.includes("broker.publish"));
  assert.ok(publishLine, "slow handler publish must complete despite shutdown");
  const frame = JSON.parse(publishLine);
  assert.equal(frame.params.event.payload.ack, true);
});
