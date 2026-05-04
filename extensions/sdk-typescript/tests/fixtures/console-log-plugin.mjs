// Fixture that calls console.log AFTER PluginAdapter is
// constructed. The default-on stdout guard must divert the
// console.log line to stderr tagged with STDOUT_GUARD_MARKER,
// keeping the JSON-RPC stream clean for the host.
import { PluginAdapter } from "../../dist/index.js";

const MANIFEST = `
[plugin]
id = "noisy_plugin"
version = "0.1.0"
name = "Noisy"
description = "fixture"
min_nexo_version = ">=0.1.0"
`;

const adapter = new PluginAdapter({
  manifestToml: MANIFEST,
  handleProcessSignals: false,
});

// Trigger the guard: this line is NOT valid JSON, so it must
// be diverted to stderr.
console.log("hello-from-noisy-plugin");

await adapter.run();
