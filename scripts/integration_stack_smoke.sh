#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

require_cmd docker
require_cmd curl
require_cmd rg

echo "[1/9] checking docker compose service health..."
status_table=""
for _ in {1..90}; do
  status_table="$(docker compose ps --format 'table {{.Service}}\t{{.Status}}')"
  all_healthy=1
  for svc in agent chrome nats; do
    line="$(printf '%s\n' "$status_table" | rg "^${svc}\\s+" || true)"
    if [[ -z "$line" || "$line" != *"(healthy)"* ]]; then
      all_healthy=0
      break
    fi
  done
  if [[ "$all_healthy" -eq 1 ]]; then
    break
  fi
  sleep 1
done
echo "$status_table"
for svc in agent chrome nats; do
  line="$(printf '%s\n' "$status_table" | rg "^${svc}\\s+" || true)"
  if [[ -z "$line" ]]; then
    echo "service not found in compose ps: $svc" >&2
    exit 1
  fi
  if [[ "$line" != *"(healthy)"* ]]; then
    echo "service is not healthy: $svc" >&2
    exit 1
  fi
done

echo "[2/9] checking /health and /ready..."
health_body="$(curl -fsS http://127.0.0.1:8080/health)"
ready_body="$(curl -fsS http://127.0.0.1:8080/ready)"
[[ "$health_body" == *'"status":"ok"'* ]] || {
  echo "unexpected /health response: $health_body" >&2
  exit 1
}
[[ "$ready_body" == *'"status":"ready"'* ]] || {
  echo "unexpected /ready response: $ready_body" >&2
  exit 1
}
# /ready must report at least one running agent — otherwise the process is up
# but no AgentRuntime actually subscribed.
if ! printf '%s\n' "$ready_body" | rg -q '"agents_running":[1-9][0-9]*'; then
  echo "/ready did not report any running agents: $ready_body" >&2
  exit 1
fi

echo "[3/9] checking /metrics..."
metrics="$(curl -fsS http://127.0.0.1:9090/metrics)"
for metric in llm_requests_total llm_latency_ms_count messages_processed_total circuit_breaker_state; do
  printf '%s\n' "$metrics" | rg -q "^${metric}" || {
    echo "missing metric in /metrics: $metric" >&2
    exit 1
  }
done
# Every series emitted by the agent must carry labels — flat series without
# braces would indicate a regression to the pre-9.2 telemetry layout.
for labeled in \
  'messages_processed_total\{agent="' \
  'circuit_breaker_state\{breaker="nats"\}'; do
  printf '%s\n' "$metrics" | rg -q "${labeled}" || {
    echo "missing labeled series matching: ${labeled}" >&2
    echo "----- /metrics body -----" >&2
    printf '%s\n' "$metrics" >&2
    exit 1
  }
done
# Prometheus TYPE/HELP lines must stay alongside the series for scrapers that
# require them; a missing TYPE line typically means a code path forgot to render.
for needed in \
  '^# TYPE llm_requests_total counter$' \
  '^# TYPE llm_latency_ms histogram$' \
  '^# TYPE messages_processed_total counter$' \
  '^# TYPE circuit_breaker_state gauge$'; do
  printf '%s\n' "$metrics" | rg -q "${needed}" || {
    echo "missing TYPE line: ${needed}" >&2
    exit 1
  }
done

echo "[4/9] checking NATS monitor endpoint..."
nats_health="$(curl -fsS http://127.0.0.1:8222/healthz)"
[[ "$nats_health" == *"ok"* ]] || {
  echo "unexpected NATS /healthz response: $nats_health" >&2
  exit 1
}

echo "[5/9] running browser E2E against compose chrome service..."
if ! command -v cargo >/dev/null 2>&1; then
  echo "  cargo not available on host — skipping browser E2E" >&2
else
  chrome_container="$(docker compose ps -q chrome)"
  if [[ -z "$chrome_container" ]]; then
    echo "  chrome container id not found — skipping browser E2E" >&2
  else
    # chrome is not port-mapped to host; reach it via its network IP instead.
    chrome_ip="$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{"\n"}}{{end}}' "$chrome_container" | rg -v '^$' | head -1)"
    if [[ -z "$chrome_ip" ]]; then
      echo "  could not resolve chrome container IP — skipping browser E2E" >&2
    else
      echo "  chrome reachable at http://${chrome_ip}:9222"
      CDP_URL="http://${chrome_ip}:9222" \
        cargo test -p agent-plugin-browser --test browser_cdp_e2e -- --nocapture
    fi
  fi
fi

echo "[6/9] verifying NATS restart recovery..."
# Restart the NATS container and verify that the agent's circuit breaker trips,
# then closes again once NATS is back. This exercises the reconnect + disk
# queue drain path from crates/broker/src/nats.rs::spawn_state_monitor.
initial_state="$(curl -fsS http://127.0.0.1:9090/metrics | rg '^circuit_breaker_state\{breaker="nats"\}' | awk '{print $2}')"
if [[ "$initial_state" != "0" ]]; then
  echo "  expected nats breaker closed before restart, got: $initial_state" >&2
  exit 1
fi

docker compose restart nats >/dev/null

# Wait up to 20s for the breaker to open or NATS to come back. We accept either
# ordering: if the agent detects the drop fast enough we will see state=1,
# but if NATS recovers before the 500ms state monitor tick, we may never see it.
tripped=0
for _ in {1..40}; do
  state="$(curl -fsS http://127.0.0.1:9090/metrics 2>/dev/null | rg '^circuit_breaker_state\{breaker="nats"\}' | awk '{print $2}')"
  if [[ "$state" == "1" ]]; then
    tripped=1
    break
  fi
  ready_code="$(curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/ready || true)"
  if [[ "$ready_code" == "503" ]]; then
    tripped=1
    break
  fi
  sleep 0.5
done
if [[ "$tripped" -eq 0 ]]; then
  echo "  warning: did not observe broker down state during NATS restart (recovery was too fast)" >&2
fi

# Wait for NATS to report healthy again.
for _ in {1..60}; do
  status="$(docker compose ps --format 'table {{.Service}}\t{{.Status}}' | rg '^nats\s+' || true)"
  [[ "$status" == *"(healthy)"* ]] && break
  sleep 1
done
[[ "$status" == *"(healthy)"* ]] || {
  echo "  NATS did not return to healthy after restart: $status" >&2
  exit 1
}

# Allow a few seconds for the agent's state monitor to observe the reconnect.
recovered=0
for _ in {1..30}; do
  state="$(curl -fsS http://127.0.0.1:9090/metrics 2>/dev/null | rg '^circuit_breaker_state\{breaker="nats"\}' | awk '{print $2}')"
  ready_code="$(curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/ready || true)"
  if [[ "$state" == "0" && "$ready_code" == "200" ]]; then
    recovered=1
    break
  fi
  sleep 1
done
if [[ "$recovered" -eq 0 ]]; then
  echo "  agent did not recover to ready + breaker closed within 30s" >&2
  echo "  last state=$state, last ready_code=$ready_code" >&2
  exit 1
fi
echo "  breaker closed, /ready=200 after restart"

echo "[7/9] running agent-to-agent delegation E2E..."
# Publish an AgentPayload::Delegate to agent.route.kate and verify the runtime
# routes a Result back on the reply topic. Independent of LLM credentials — the
# runtime wraps any LLM error as {"error": ...} in the Result payload.
if ! command -v cargo >/dev/null 2>&1; then
  echo "  cargo not available on host — skipping delegation E2E" >&2
else
  NATS_URL="nats://127.0.0.1:4222" \
    cargo test -p agent-core --test delegation_e2e_test -- --nocapture
fi

echo "[8/9] running disk queue drain E2E..."
# Exercises DiskQueue::drain_nats against the real NATS broker. Simulates the
# recovery path where events buffered during a disconnect get published and
# removed from pending_events once the client reconnects.
if ! command -v cargo >/dev/null 2>&1; then
  echo "  cargo not available on host — skipping drain E2E" >&2
else
  NATS_URL="nats://127.0.0.1:4222" \
    cargo test -p agent-broker --test disk_queue_drain_nats_test -- --nocapture
fi

echo "[9/9] running ExtensionDirectory NATS E2E..."
# Verifies ExtensionDirectory against a real async-nats broker path (not
# LocalBroker): announce -> runtime connect/initialize -> Added event ->
# shutdown beacon -> Removed event.
if ! command -v cargo >/dev/null 2>&1; then
  echo "  cargo not available on host — skipping extension directory NATS E2E" >&2
else
  NATS_URL="nats://127.0.0.1:4222" \
    cargo test -p agent-extensions --test directory_nats_e2e_test -- --nocapture
fi

echo "integration smoke checks passed"
