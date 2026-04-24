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
require_cmd cargo

echo "[0/6] ensuring compose stack is up"
docker compose up -d >/dev/null

echo "[1/6] pre-check: stack health"
./scripts/integration_stack_smoke.sh

echo "[2/6] pre-check: browser NATS roundtrip"
cargo run --quiet --bin integration-browser-check

echo "[3/6] stopping nats to force not-ready state"
docker compose stop nats >/dev/null

seen_not_ready=0
for _ in {1..20}; do
  code="$(curl -sS -o /dev/null -w "%{http_code}" http://127.0.0.1:8080/ready || true)"
  if [[ "$code" != "200" ]]; then
    seen_not_ready=1
    break
  fi
  sleep 1
done
if [[ "$seen_not_ready" -ne 1 ]]; then
  echo "ready endpoint never dropped after nats stop" >&2
  exit 1
fi

echo "[4/6] starting nats and waiting for healthy"
docker compose start nats >/dev/null
for _ in {1..90}; do
  line="$(docker compose ps --format 'table {{.Service}}\t{{.Status}}' | rg '^nats\s+' || true)"
  if [[ -n "$line" && "$line" == *"(healthy)"* ]]; then
    break
  fi
  sleep 1
done
line="$(docker compose ps --format 'table {{.Service}}\t{{.Status}}' | rg '^nats\s+' || true)"
if [[ -z "$line" || "$line" != *"(healthy)"* ]]; then
  echo "nats did not become healthy after restart" >&2
  exit 1
fi

echo "[5/6] waiting for agent readiness recovery"
for _ in {1..90}; do
  code="$(curl -sS -o /dev/null -w "%{http_code}" http://127.0.0.1:8080/ready || true)"
  if [[ "$code" == "200" ]]; then
    break
  fi
  sleep 1
done
code="$(curl -sS -o /dev/null -w "%{http_code}" http://127.0.0.1:8080/ready || true)"
if [[ "$code" != "200" ]]; then
  echo "agent readiness did not recover (last status: $code)" >&2
  exit 1
fi

echo "[6/6] post-check: browser NATS roundtrip after recovery"
cargo run --quiet --bin integration-browser-check

echo "nats recovery integration checks passed"
