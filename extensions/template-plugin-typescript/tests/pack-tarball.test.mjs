/**
 * Phase 31.5 — end-to-end test of `scripts/pack-tarball-typescript.sh`.
 *
 * Asserts the bash pipeline produces a tarball whose name +
 * layout + sha256 sidecar match the convention 31.1 consumes.
 *
 * Synthetic SDK: a tempdir with a stub `dist/index.js` + a
 * `package.json` substitutes for the real SDK. SKIP_BUILD=1 +
 * SKIP_NPM=1 env overrides bypass the heavy `tsc` + `npm`
 * steps so the test is deterministic + fast.
 */

import { test } from "node:test";
import { strict as assert } from "node:assert";
import { mkdirSync, mkdtempSync, copyFileSync, cpSync, writeFileSync, statSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { Readable } from "node:stream";
import { execSync } from "node:child_process";

const HERE = dirname(fileURLToPath(import.meta.url));
const TEMPLATE_ROOT = dirname(HERE);
const PLUGIN_ID = "template_plugin_typescript";
const PLUGIN_VERSION = "0.1.0";

test("pack_tarball_produces_canonical_layout", async () => {
  const work = mkdtempSync(join(tmpdir(), "pack-ts-"));
  const sdk = mkdtempSync(join(tmpdir(), "sdk-stub-"));
  const extractDir = mkdtempSync(join(tmpdir(), "pack-ts-extract-"));

  // 1. Synthetic SDK with a stub dist/index.js.
  mkdirSync(join(sdk, "dist"), { recursive: true });
  writeFileSync(join(sdk, "dist", "index.js"), "// stub SDK\nexport const VERSION = '0.0.0';\n");
  writeFileSync(join(sdk, "package.json"), JSON.stringify({
    name: "nexo-plugin-sdk",
    version: "0.0.0",
    main: "./dist/index.js",
    type: "module",
  }));

  // 2. Copy template fixture (manifest + scripts) into work dir.
  copyFileSync(join(TEMPLATE_ROOT, "nexo-plugin.toml"), join(work, "nexo-plugin.toml"));
  cpSync(join(TEMPLATE_ROOT, "scripts"), join(work, "scripts"), { recursive: true });
  // Provide a pre-built dist/main.js so the pack script can copy
  // it without running tsc.
  mkdirSync(join(work, "dist"), { recursive: true });
  writeFileSync(join(work, "dist", "main.js"), "console.error('synthetic main')\n");

  // 3. Run the pack script with SDK_SRC + SKIP_BUILD + SKIP_NPM.
  const packResult = spawnSync("bash", ["scripts/pack-tarball-typescript.sh"], {
    cwd: work,
    env: { ...process.env, SDK_SRC: sdk, SKIP_BUILD: "1", SKIP_NPM: "1" },
    encoding: "utf-8",
  });
  assert.equal(
    packResult.status,
    0,
    `pack failed: stdout=${packResult.stdout} stderr=${packResult.stderr}`,
  );

  // 4. Asset present.
  const assetName = `${PLUGIN_ID}-${PLUGIN_VERSION}-noarch.tar.gz`;
  const asset = join(work, "dist", assetName);
  const sidecar = join(work, "dist", `${assetName}.sha256`);
  assert.ok(statSync(asset).isFile(), `asset missing: ${asset}`);
  assert.ok(statSync(sidecar).isFile(), `sha sidecar missing: ${sidecar}`);

  // 5. Sidecar is 64 lowercase hex chars.
  const sidecarHex = readFileSync(sidecar, "utf-8").trim();
  assert.equal(sidecarHex.length, 64);
  assert.match(sidecarHex, /^[0-9a-f]{64}$/);

  // 6. Recompute sha256.
  const hasher = createHash("sha256");
  hasher.update(readFileSync(asset));
  assert.equal(hasher.digest("hex"), sidecarHex);

  // 7. Re-extract via system tar + verify layout.
  execSync(`tar -xzf "${asset}" -C "${extractDir}"`);
  const top = new Set(readdirSync(extractDir));
  assert.deepEqual(
    [...top].sort(),
    ["bin", "lib", "nexo-plugin.toml"].sort(),
    `unexpected top-level entries: ${[...top].join(",")}`,
  );
  assert.ok(statSync(join(extractDir, "bin", PLUGIN_ID)).isFile());
  assert.ok(statSync(join(extractDir, "lib", "plugin", "main.js")).isFile());
  assert.ok(statSync(join(extractDir, "lib", "node_modules", "nexo-plugin-sdk", "dist", "index.js")).isFile());

  // Launcher executable bit preserved.
  const mode = statSync(join(extractDir, "bin", PLUGIN_ID)).mode & 0o777;
  assert.equal(mode, 0o755, `launcher mode should be 0o755, got ${mode.toString(8)}`);
});
