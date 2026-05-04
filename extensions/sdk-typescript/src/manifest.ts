/**
 * Phase 31.5 — manifest TOML parser.
 *
 * Validates only the fields the SDK needs at construction time.
 * The daemon performs full schema validation when it loads the
 * manifest at boot, so this stays minimal.
 */

import { parse as parseToml } from "smol-toml";

import { ManifestError } from "./errors.js";

const PLUGIN_ID_REGEX = /^[a-z][a-z0-9_]{0,31}$/;

export interface ManifestPluginSection {
  id: string;
  version: string;
  name: string;
  description: string;
}

export interface ParsedManifest {
  plugin: ManifestPluginSection;
  /** Full TOML document for callers that need extra fields. */
  raw: Record<string, unknown>;
}

function asPlainObject(v: unknown): Record<string, unknown> | null {
  return typeof v === "object" && v !== null && !Array.isArray(v)
    ? (v as Record<string, unknown>)
    : null;
}

function requireString(
  obj: Record<string, unknown>,
  field: string,
  ownerLabel: string,
): string {
  const value = obj[field];
  if (typeof value !== "string" || value.length === 0) {
    throw new ManifestError(
      `${ownerLabel}.${field} is missing or not a non-empty string`,
      `${ownerLabel}.${field}`,
    );
  }
  return value;
}

export function parseManifest(toml: string): ParsedManifest {
  let raw: Record<string, unknown>;
  try {
    raw = parseToml(toml) as Record<string, unknown>;
  } catch (e) {
    const reason = e instanceof Error ? e.message : String(e);
    throw new ManifestError(`manifest TOML parse failed: ${reason}`);
  }

  const pluginObj = asPlainObject(raw["plugin"]);
  if (pluginObj === null) {
    throw new ManifestError("manifest is missing the [plugin] section", "plugin");
  }

  const id = requireString(pluginObj, "id", "plugin");
  if (!PLUGIN_ID_REGEX.test(id)) {
    throw new ManifestError(
      `plugin.id "${id}" must match /^[a-z][a-z0-9_]{0,31}$/`,
      "plugin.id",
    );
  }
  const version = requireString(pluginObj, "version", "plugin");
  const name = requireString(pluginObj, "name", "plugin");
  const description = requireString(pluginObj, "description", "plugin");

  return {
    plugin: { id, version, name, description },
    raw,
  };
}
