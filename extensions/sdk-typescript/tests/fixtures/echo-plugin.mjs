// Echo fixture for dispatch tests. Reads MANIFEST + behavior
// flags from env so the same fixture serves multiple test cases.
import { PluginAdapter, Event } from "../../dist/index.js";

const MANIFEST_DEFAULT = `
[plugin]
id = "echo_plugin"
version = "0.1.0"
name = "Echo"
description = "fixture"
min_nexo_version = ">=0.1.0"
`;

const adapter = new PluginAdapter({
  manifestToml: process.env["FIXTURE_MANIFEST"] ?? MANIFEST_DEFAULT,
  serverVersion: process.env["FIXTURE_SERVER_VERSION"] ?? "0.0.99",
  enableStdoutGuard: process.env["FIXTURE_DISABLE_GUARD"] === "1" ? false : true,
  handleProcessSignals: false,
  onEvent: async (topic, event, broker) => {
    const out = Event.new(
      "plugin.inbound.echoed",
      "echo_plugin",
      {
        echoed: event.payload,
        incoming_topic: topic,
      },
    );
    await broker.publish("plugin.inbound.echoed", out);
  },
  onShutdown: async () => {
    process.stderr.write("echo_plugin shutdown_handler invoked\n");
  },
});
await adapter.run();
