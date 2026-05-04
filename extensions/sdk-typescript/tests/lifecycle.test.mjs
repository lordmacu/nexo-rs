/**
 * Phase 31.5 — lifecycle test: calling run() twice must throw
 * PluginError. Single-shot guard.
 *
 * Driven via a child fixture so the readline loop the SDK
 * starts in `run()` does not block the test runner's stdin.
 */

import { test } from "node:test";
import { strict as assert } from "node:assert";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE = join(HERE, "fixtures", "lifecycle-plugin.mjs");

test("run_twice_throws_PluginError", async () => {
  const proc = spawn(process.execPath, [FIXTURE], { stdio: ["pipe", "pipe", "pipe"] });
  const captured = new Promise((resolve, reject) => {
    let so = "", se = "";
    proc.stdout.on("data", (d) => { so += d.toString("utf-8"); });
    proc.stderr.on("data", (d) => { se += d.toString("utf-8"); });
    proc.on("close", (code) => resolve({ stdout: so, stderr: se, code }));
    proc.on("error", reject);
  });
  proc.stdin.end();

  const { stdout, stderr, code } = await captured;
  assert.equal(code, 0, `expected exit 0, got ${code}; stderr=${stderr}`);
  assert.ok(stdout.includes("LIFECYCLE_TEST_OK"), `sentinel missing: stdout=${stdout} stderr=${stderr}`);
});
