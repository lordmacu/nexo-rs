/**
 * Phase 31.5 — handshake tests.
 *
 * Black-box exercise: spawn a child Node process running the
 * echo fixture, send JSON-RPC frames over stdin, assert lines
 * on stdout. The wire format is the source of truth so we do
 * not touch SDK internals.
 */

import { test } from "node:test";
import { strict as assert } from "node:assert";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE = join(HERE, "fixtures", "echo-plugin.mjs");

function jsonrpcRequest(id, method, params) {
  const frame = { jsonrpc: "2.0", id, method };
  if (params !== undefined) frame.params = params;
  return JSON.stringify(frame) + "\n";
}

function readStdoutLines(proc) {
  return new Promise((resolve, reject) => {
    let buf = "";
    let stderrBuf = "";
    proc.stdout.on("data", (d) => { buf += d.toString("utf-8"); });
    proc.stderr.on("data", (d) => { stderrBuf += d.toString("utf-8"); });
    proc.on("close", () => resolve({ stdout: buf, stderr: stderrBuf }));
    proc.on("error", reject);
  });
}

test("initialize_returns_manifest", async () => {
  const proc = spawn(process.execPath, [FIXTURE], { stdio: ["pipe", "pipe", "pipe"] });
  const captured = readStdoutLines(proc);
  proc.stdin.write(jsonrpcRequest(1, "initialize"));
  proc.stdin.write(jsonrpcRequest(2, "shutdown"));
  proc.stdin.end();

  const { stdout } = await captured;
  const lines = stdout.split("\n").filter((l) => l.trim().length > 0);
  assert.ok(lines.length >= 2, `expected ≥2 reply lines, got ${JSON.stringify(lines)}`);

  const first = JSON.parse(lines[0]);
  assert.equal(first.jsonrpc, "2.0");
  assert.equal(first.id, 1);
  assert.equal(first.result.server_version, "0.0.99");
  assert.equal(first.result.manifest.plugin.id, "echo_plugin");

  const second = JSON.parse(lines[1]);
  assert.equal(second.id, 2);
  assert.equal(second.result.ok, true);
});

test("unknown_method_returns_error_minus_32601", async () => {
  const proc = spawn(process.execPath, [FIXTURE], { stdio: ["pipe", "pipe", "pipe"] });
  const captured = readStdoutLines(proc);
  proc.stdin.write(jsonrpcRequest(7, "garbage.method"));
  proc.stdin.write(jsonrpcRequest(8, "shutdown"));
  proc.stdin.end();

  const { stdout } = await captured;
  const lines = stdout.split("\n").filter((l) => l.trim().length > 0);
  const first = JSON.parse(lines[0]);
  assert.equal(first.id, 7);
  assert.ok(first.error, `expected error response, got ${JSON.stringify(first)}`);
  assert.equal(first.error.code, -32601);
});

test("unknown_notification_silently_ignored", async () => {
  const proc = spawn(process.execPath, [FIXTURE], { stdio: ["pipe", "pipe", "pipe"] });
  const captured = readStdoutLines(proc);
  // No `id` → notification; SDK must NOT reply.
  proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "garbage.notif" }) + "\n");
  proc.stdin.write(jsonrpcRequest(9, "shutdown"));
  proc.stdin.end();

  const { stdout } = await captured;
  const lines = stdout.split("\n").filter((l) => l.trim().length > 0);
  // Only the shutdown reply should appear.
  assert.equal(lines.length, 1, `expected 1 line (shutdown reply only), got ${JSON.stringify(lines)}`);
  assert.equal(JSON.parse(lines[0]).id, 9);
});
