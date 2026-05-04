/**
 * Phase 31.5 — JSON-RPC 2.0 frame types + wire helpers.
 *
 * Frames are serialized as one JSON object per line, terminated
 * by `\n`. The host's reader uses readline-style line buffering;
 * the child does the same via Node's `readline.createInterface`.
 */

export const JSONRPC_VERSION = "2.0";

/** Maximum byte length of a single JSON-RPC frame the SDK accepts.
 * Frames larger than this are rejected with a WireError so an
 * adversarial host cannot OOM the plugin via a single huge line.
 */
export const MAX_FRAME_BYTES = 1 << 20; // 1 MiB

export type JsonRpcId = number | string | null;

export interface JsonRpcRequest<P = unknown> {
  jsonrpc: typeof JSONRPC_VERSION;
  id: JsonRpcId;
  method: string;
  params?: P;
}

export interface JsonRpcNotification<P = unknown> {
  jsonrpc: typeof JSONRPC_VERSION;
  method: string;
  params?: P;
}

export interface JsonRpcResponse<R = unknown> {
  jsonrpc: typeof JSONRPC_VERSION;
  id: JsonRpcId;
  result: R;
}

export interface JsonRpcErrorResponse {
  jsonrpc: typeof JSONRPC_VERSION;
  id: JsonRpcId;
  error: { code: number; message: string; data?: unknown };
}

export type JsonRpcFrame =
  | JsonRpcRequest
  | JsonRpcNotification
  | JsonRpcResponse
  | JsonRpcErrorResponse;

/** Serialize a frame to a single line terminated by `\n`. */
export function serializeFrame(frame: JsonRpcFrame): string {
  return JSON.stringify(frame) + "\n";
}

/** Build a typed response. */
export function buildResponse<R>(id: JsonRpcId, result: R): JsonRpcResponse<R> {
  return { jsonrpc: JSONRPC_VERSION, id, result };
}

/** Build an error response. */
export function buildErrorResponse(
  id: JsonRpcId,
  code: number,
  message: string,
): JsonRpcErrorResponse {
  return { jsonrpc: JSONRPC_VERSION, id, error: { code, message } };
}
