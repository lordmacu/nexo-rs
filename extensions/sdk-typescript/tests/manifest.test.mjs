/**
 * Phase 31.5 — manifest validation tests. In-process — no spawn.
 */

import { test } from "node:test";
import { strict as assert } from "node:assert";

import { parseManifest, ManifestError } from "../dist/index.js";

test("missing_id_throws_ManifestError_with_field", () => {
  const toml = `[plugin]
version = "0.1.0"
name = "x"
description = "y"`;
  try {
    parseManifest(toml);
    assert.fail("expected ManifestError");
  } catch (e) {
    assert.ok(e instanceof ManifestError, `expected ManifestError, got ${e.name}`);
    assert.equal(e.field, "plugin.id");
  }
});

test("invalid_toml_throws_ManifestError", () => {
  const toml = `[[[unterminated`;
  try {
    parseManifest(toml);
    assert.fail("expected ManifestError");
  } catch (e) {
    assert.ok(e instanceof ManifestError, `got ${e.name}`);
  }
});

test("id_regex_violation_throws_ManifestError", () => {
  const toml = `[plugin]
id = "Bad-Id"
version = "0.1.0"
name = "x"
description = "y"`;
  try {
    parseManifest(toml);
    assert.fail("expected ManifestError");
  } catch (e) {
    assert.ok(e instanceof ManifestError, `got ${e.name}`);
    assert.equal(e.field, "plugin.id");
  }
});
