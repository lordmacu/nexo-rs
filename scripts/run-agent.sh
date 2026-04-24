#!/usr/bin/env bash
# Run the agent binary, killing any previous instance first.
# Writes combined stdout/stderr to agent.log and also to the terminal.
set -euo pipefail

cd "$(dirname "$0")/.."

BIN="./target/debug/agent"

# Kill any prior instance bound to our binary. Exclude self + grep.
pids=$(pgrep -f "$BIN" || true)
if [[ -n "$pids" ]]; then
  echo "killing previous agent(s): $pids" >&2
  kill $pids 2>/dev/null || true
  sleep 1
  # Force-kill any that didn't exit
  pids=$(pgrep -f "$BIN" || true)
  if [[ -n "$pids" ]]; then
    echo "force-killing: $pids" >&2
    kill -9 $pids 2>/dev/null || true
    sleep 1
  fi
fi

exec "$BIN" --config ./config "$@" 2>&1 | tee agent.log
