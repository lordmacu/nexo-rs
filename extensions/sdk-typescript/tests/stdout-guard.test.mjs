/**
 * Phase 31.5 — stdout guard tests. Mix of in-process unit tests
 * and a spawn-fixture end-to-end check.
 */

import { test } from "node:test";
import { strict as assert } from "node:assert";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import {
  installStdoutGuard,
  uninstallStdoutGuard,
  isStdoutGuardInstalled,
  STDOUT_GUARD_MARKER,
} from "../dist/index.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const NOISY = join(HERE, "fixtures", "console-log-plugin.mjs");

test("idempotent_install", () => {
  // Second call must be a no-op; subsequent uninstalls bring the
  // process back to the original write impl.
  const wasInstalled = isStdoutGuardInstalled();
  installStdoutGuard();
  installStdoutGuard();
  assert.equal(isStdoutGuardInstalled(), true);
  uninstallStdoutGuard();
  assert.equal(isStdoutGuardInstalled(), false);
  // Nesting: re-install + re-uninstall.
  installStdoutGuard();
  uninstallStdoutGuard();
  assert.equal(isStdoutGuardInstalled(), wasInstalled);
});

test("diverts_non_json_console_log_to_stderr", async () => {
  // The fixture calls console.log("hello-from-noisy-plugin")
  // BEFORE entering the dispatch loop. With the guard installed
  // (default-on) the line goes to stderr tagged with the
  // sentinel, NOT stdout — keeping the JSON-RPC stream clean.
  const proc = spawn(process.execPath, [NOISY], { stdio: ["pipe", "pipe", "pipe"] });
  const captured = new Promise((resolve, reject) => {
    let so = "", se = "";
    proc.stdout.on("data", (d) => { so += d.toString("utf-8"); });
    proc.stderr.on("data", (d) => { se += d.toString("utf-8"); });
    proc.on("close", () => resolve({ stdout: so, stderr: se }));
    proc.on("error", reject);
  });
  // Drive the fixture through a complete handshake so it exits.
  proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "shutdown" }) + "\n");
  proc.stdin.end();

  const { stdout, stderr } = await captured;
  // Non-JSON line must NOT appear on stdout (guard diverted it).
  assert.ok(
    !stdout.includes("hello-from-noisy-plugin"),
    `non-JSON line leaked to stdout: ${stdout}`,
  );
  // Must appear on stderr tagged with the sentinel.
  assert.ok(
    stderr.includes(`${STDOUT_GUARD_MARKER} hello-from-noisy-plugin`),
    `guarded line missing on stderr: ${stderr}`,
  );
  // The shutdown reply (valid JSON) must be visible on stdout.
  assert.ok(stdout.includes('"ok":true'), `shutdown reply missing from stdout: ${stdout}`);
});
