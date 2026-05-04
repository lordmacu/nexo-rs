/**
 * Phase 31.5 — public API for the TypeScript plugin SDK.
 *
 * Mirrors the Rust SDK in `crates/microapp-sdk/` and the Python
 * SDK in `extensions/sdk-python/nexo_plugin_sdk/`.
 */

export { PluginAdapter } from "./adapter.js";
export type {
  EventHandler,
  ShutdownHandler,
  PluginAdapterOptions,
} from "./adapter.js";
export { BrokerSender } from "./broker.js";
export type { LineWriter } from "./broker.js";
export { Event } from "./events.js";
export {
  PluginError,
  ManifestError,
  WireError,
} from "./errors.js";
export { parseManifest } from "./manifest.js";
export type { ManifestPluginSection, ParsedManifest } from "./manifest.js";
export {
  installStdoutGuard,
  uninstallStdoutGuard,
  isStdoutGuardInstalled,
  STDOUT_GUARD_MARKER,
} from "./stdout-guard.js";
export {
  JSONRPC_VERSION,
  MAX_FRAME_BYTES,
  serializeFrame,
  buildResponse,
  buildErrorResponse,
} from "./wire.js";
export type {
  JsonRpcId,
  JsonRpcRequest,
  JsonRpcNotification,
  JsonRpcResponse,
  JsonRpcErrorResponse,
  JsonRpcFrame,
} from "./wire.js";

export const VERSION = "0.1.0";
