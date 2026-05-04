/**
 * Phase 31.5 — TypeScript plugin template entrypoint.
 *
 * Mirrors the structure of `extensions/template-plugin-python/src/main.py`
 * and the Rust template's `src/main.rs`: parse the bundled
 * manifest, build a PluginAdapter, register an echo handler,
 * drive the dispatch loop until the daemon sends `shutdown`.
 *
 * Replace `onEvent` with your own channel logic. Topics on the
 * allowlist for this plugin (per the manifest's
 * `[[plugin.channels.register]]` entry) are
 * `plugin.outbound.template_echo_ts[.<instance>]` for inbound
 * events from the daemon and
 * `plugin.inbound.template_echo_ts[.<instance>]` for messages
 * you send back through `broker.publish(...)`.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

import { PluginAdapter, Event } from "nexo-plugin-sdk";

// When run from a packed tarball, `bin/<id>` sets
// NODE_PATH=lib/node_modules + exec's `node lib/plugin/main.js`.
// The manifest sits at `<plugin_dir>/nexo-plugin.toml`; we walk
// up two parents from `lib/plugin/main.js` to reach it. In dev
// mode (`node dist/main.js` from the template root) the
// manifest is also at `../nexo-plugin.toml`.
const HERE = dirname(fileURLToPath(import.meta.url));
const MANIFEST_PATH = resolve(HERE, "..", "..", "nexo-plugin.toml");
let MANIFEST_TOML: string;
try {
  MANIFEST_TOML = readFileSync(MANIFEST_PATH, "utf-8");
} catch {
  // Dev fallback: try one level up (common when running from
  // `extensions/template-plugin-typescript/` directly via
  // `node dist/main.js`).
  MANIFEST_TOML = readFileSync(resolve(HERE, "..", "nexo-plugin.toml"), "utf-8");
}

const adapter = new PluginAdapter({
  manifestToml: MANIFEST_TOML,
  serverVersion: "0.1.0",
  onEvent: async (topic, event, broker) => {
    const outTopic = topic.startsWith("plugin.outbound.")
      ? `plugin.inbound.${topic.slice("plugin.outbound.".length)}`
      : `plugin.inbound.${topic}`;
    const out = Event.new(
      outTopic,
      event.source ?? "template_plugin_typescript",
      { echoed: event.payload, incoming_topic: topic },
    );
    try {
      await broker.publish(outTopic, out);
    } catch (e) {
      const reason = e instanceof Error ? e.message : String(e);
      process.stderr.write(`plugin: publish failed: ${reason}\n`);
    }
  },
  onShutdown: async () => {
    process.stderr.write("template_plugin_typescript: shutdown requested\n");
  },
});

await adapter.run();
