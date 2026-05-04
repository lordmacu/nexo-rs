// Slow handler fixture: handler awaits 200ms before publishing
// its reply. Used to assert in-flight tasks are drained on
// shutdown (no mid-publish cancellation).
import { PluginAdapter, Event } from "../../dist/index.js";

const MANIFEST = `
[plugin]
id = "slow_plugin"
version = "0.1.0"
name = "Slow"
description = "fixture"
min_nexo_version = ">=0.1.0"
`;

const adapter = new PluginAdapter({
  manifestToml: MANIFEST,
  handleProcessSignals: false,
  onEvent: async (topic, event, broker) => {
    await new Promise((r) => setTimeout(r, 200));
    void event;
    void topic;
    const out = Event.new("plugin.inbound.slow", "slow_plugin", { ack: true });
    await broker.publish("plugin.inbound.slow", out);
  },
});
await adapter.run();
