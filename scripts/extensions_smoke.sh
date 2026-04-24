#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label (missing '$needle')"
  fi
}

rpc_call() {
  local bin="$1"
  local payload="$2"
  printf '%s\n' "$payload" | "$bin"
}

echo "==> Building extension binaries"
cargo build --release --manifest-path "$ROOT_DIR/extensions/weather/Cargo.toml"
cargo build --release --manifest-path "$ROOT_DIR/extensions/openai-whisper/Cargo.toml"
cargo build --release --manifest-path "$ROOT_DIR/extensions/summarize/Cargo.toml"
cargo build --release --manifest-path "$ROOT_DIR/extensions/goplaces/Cargo.toml"
cargo build --release --manifest-path "$ROOT_DIR/extensions/openstreetmap/Cargo.toml"
cargo build --release --manifest-path "$ROOT_DIR/extensions/github/Cargo.toml"

echo "==> Checking initialize handshake"
out="$(rpc_call "$ROOT_DIR/extensions/weather/target/release/weather" '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}')"
assert_contains "$out" '"server_version":"weather-0.1.0"' "weather initialize"

out="$(rpc_call "$ROOT_DIR/extensions/openai-whisper/target/release/openai-whisper" '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}')"
assert_contains "$out" '"server_version":"openai-whisper-0.1.0"' "openai-whisper initialize"

out="$(rpc_call "$ROOT_DIR/extensions/summarize/target/release/summarize" '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}')"
assert_contains "$out" '"server_version":"summarize-0.1.0"' "summarize initialize"

out="$(rpc_call "$ROOT_DIR/extensions/goplaces/target/release/goplaces" '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}')"
assert_contains "$out" '"server_version":"goplaces-0.1.0"' "goplaces initialize"

out="$(rpc_call "$ROOT_DIR/extensions/openstreetmap/target/release/openstreetmap" '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}')"
assert_contains "$out" '"server_version":"openstreetmap-0.1.0"' "openstreetmap initialize"

out="$(rpc_call "$ROOT_DIR/extensions/github/target/release/github" '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}')"
assert_contains "$out" '"server_version":"github-0.1.0"' "github initialize"

echo "==> Checking status tools"
out="$(rpc_call "$ROOT_DIR/extensions/weather/target/release/weather" '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}')"
assert_contains "$out" '"provider":"wttr.in"' "weather status"

out="$(rpc_call "$ROOT_DIR/extensions/openai-whisper/target/release/openai-whisper" '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}')"
assert_contains "$out" '"provider":"openai"' "openai-whisper status"

out="$(rpc_call "$ROOT_DIR/extensions/summarize/target/release/summarize" '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}')"
assert_contains "$out" '"provider":"openai-compatible"' "summarize status"

out="$(rpc_call "$ROOT_DIR/extensions/goplaces/target/release/goplaces" '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}')"
assert_contains "$out" '"provider_default":"auto"' "goplaces status"

out="$(rpc_call "$ROOT_DIR/extensions/openstreetmap/target/release/openstreetmap" '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}')"
assert_contains "$out" '"provider":"openstreetmap-nominatim"' "openstreetmap status"

out="$(rpc_call "$ROOT_DIR/extensions/github/target/release/github" '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}')"
assert_contains "$out" '"gh_version"' "github status"

echo "==> Checking expected configuration errors (no keys/network assumptions)"
out="$(rpc_call "$ROOT_DIR/extensions/openai-whisper/target/release/openai-whisper" '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"transcribe_file","arguments":{"file_path":"/tmp/does-not-exist.wav"}}}')"
assert_contains "$out" '"error"' "openai-whisper missing file"

out="$(rpc_call "$ROOT_DIR/extensions/summarize/target/release/summarize" '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"summarize_text","arguments":{"text":"hola"}}}')"
assert_contains "$out" 'OPENAI_API_KEY is missing' "summarize missing key"

out="$(rpc_call "$ROOT_DIR/extensions/goplaces/target/release/goplaces" '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_text","arguments":{"query":"coffee madrid","provider":"google"}}}')"
assert_contains "$out" 'GOOGLE_PLACES_API_KEY is missing' "goplaces forced google missing key"

echo "extensions smoke checks passed"
