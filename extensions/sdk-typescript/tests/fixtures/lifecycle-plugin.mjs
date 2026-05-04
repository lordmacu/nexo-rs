// Lifecycle fixture: starts run() once, then immediately
// invokes it again. Second call must reject with PluginError;
// fixture writes a sentinel on stdout and exits.
import { PluginAdapter, PluginError } from "../../dist/index.js";

const MANIFEST = `
[plugin]
id = "lifecycle_plugin"
version = "0.1.0"
name = "Lifecycle"
description = "fixture"
min_nexo_version = ">=0.1.0"
`;

const adapter = new PluginAdapter({
  manifestToml: MANIFEST,
  enableStdoutGuard: false,
  handleProcessSignals: false,
});

// First run kicks off the readline loop. Catch its eventual
// rejection silently (when stdin closes naturally we get a
// resolve, not a reject; but if some upstream throws we don't
// want an unhandled rejection).
const first = adapter.run().catch(() => {});
void first;

// Second run must reject synchronously (sync throw inside an
// async fn produces a rejected Promise on the same tick).
try {
  await adapter.run();
  process.stderr.write("LIFECYCLE_TEST_FAIL: second run resolved instead of rejecting\n");
  process.exit(2);
} catch (e) {
  if (e instanceof PluginError && /already invoked/.test(e.message)) {
    process.stdout.write("LIFECYCLE_TEST_OK\n");
    process.exit(0);
  }
  process.stderr.write(`LIFECYCLE_TEST_FAIL: wrong error: ${e?.name}: ${e?.message}\n`);
  process.exit(3);
}
