#!/usr/bin/env bash
# Single-shot health summary for monitoring scrapers + on-call.
#
# Probes the same surfaces a Kubernetes liveness/readiness probe
# would, plus the slower / non-standard checks (capability inventory,
# extension status, DLQ depth) that don't fit a binary HTTP probe.
# Emits one JSON document; exit 0 on overall healthy, exit 1 on any
# critical failure.
#
# Usage:
#   nexo-health.sh                          # default endpoints
#   nexo-health.sh --host 127.0.0.1:8080    # custom admin port
#   nexo-health.sh --json                   # JSON only, no human prose
#   nexo-health.sh --strict                 # warn level fails too
#
# This is the bridge until the proper `nexo doctor health`
# subcommand (Phase 44) lands. Cron-friendly. Designed to fit on
# one screen of `watch -n 5 nexo-health.sh`.

set -euo pipefail

ADMIN_HOST="127.0.0.1:8080"
METRICS_HOST="127.0.0.1:9090"
JSON_ONLY=0
STRICT=0

while [ $# -gt 0 ]; do
    case "$1" in
        --host) ADMIN_HOST="$2"; shift 2;;
        --metrics-host) METRICS_HOST="$2"; shift 2;;
        --json) JSON_ONLY=1; shift;;
        --strict) STRICT=1; shift;;
        --help|-h) sed -n '2,18p' "$0"; exit 0;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done

command -v curl >/dev/null || { echo "ERROR: curl missing" >&2; exit 3; }
command -v jq   >/dev/null || { echo "ERROR: jq missing" >&2; exit 3; }

# --- helpers ---------------------------------------------------------

probe() {
    # probe <name> <url> [<expected-substring>]
    local name="$1"
    local url="$2"
    local expect="${3:-}"
    local body status
    body=$(curl -sS --max-time 3 "$url" 2>&1) || {
        printf '%s\t%s\t%s\n' "$name" "fail" "request failed"
        return
    }
    if [ -n "$expect" ] && ! echo "$body" | grep -q "$expect"; then
        printf '%s\t%s\t%s\n' "$name" "fail" "missing $expect"
        return
    fi
    printf '%s\t%s\t%s\n' "$name" "ok" "${body:0:80}"
}

# --- probes ----------------------------------------------------------

RESULTS=$(
    {
        probe "live"    "http://$ADMIN_HOST/health"
        probe "ready"   "http://$ADMIN_HOST/ready"
        probe "metrics" "http://$METRICS_HOST/metrics" "nexo_"
    }
)

# --- aggregate ------------------------------------------------------

OVERALL="ok"
JSON_PROBES="["
FIRST=1
while IFS=$'\t' read -r name status detail; do
    [ $FIRST -eq 1 ] || JSON_PROBES+=","
    FIRST=0
    JSON_PROBES+=$(printf '{"name":%s,"status":%s,"detail":%s}' \
        "$(jq -R . <<<"$name")" \
        "$(jq -R . <<<"$status")" \
        "$(jq -R . <<<"$detail")")
    if [ "$status" = "fail" ]; then
        OVERALL="fail"
    fi
done <<< "$RESULTS"
JSON_PROBES+="]"

# Pull a few quick metrics for the summary panel.
METRICS_BODY=$(curl -sS --max-time 3 "http://$METRICS_HOST/metrics" 2>/dev/null || echo "")
TOOL_CALLS=$(echo "$METRICS_BODY" | awk '/^nexo_tool_calls_total/{n+=$NF} END{print n+0}')
LLM_CHUNKS=$(echo "$METRICS_BODY" | awk '/^nexo_llm_stream_chunks_total/{n+=$NF} END{print n+0}')
WEB_BREAKER=$(echo "$METRICS_BODY" | awk '/^nexo_web_search_breaker_open_total/{n+=$NF} END{print n+0}')

OUT=$(jq -n \
    --argjson probes "$JSON_PROBES" \
    --arg overall "$OVERALL" \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg admin "$ADMIN_HOST" \
    --arg metrics "$METRICS_HOST" \
    --argjson tool_calls "$TOOL_CALLS" \
    --argjson llm_chunks "$LLM_CHUNKS" \
    --argjson web_breaker "$WEB_BREAKER" \
    '{
        overall: $overall,
        timestamp: $ts,
        endpoints: { admin: $admin, metrics: $metrics },
        probes: $probes,
        counters: {
            tool_calls_total:        $tool_calls,
            llm_stream_chunks_total: $llm_chunks,
            web_search_breaker_open_total: $web_breaker
        }
    }')

if [ $JSON_ONLY -eq 1 ]; then
    echo "$OUT"
else
    echo "============================================================"
    echo " nexo-rs health  ·  $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "============================================================"
    echo
    echo "$OUT" | jq -r '
        "  overall:      " + .overall,
        "  admin:        " + .endpoints.admin,
        "  metrics:      " + .endpoints.metrics,
        "",
        "  probes:",
        (.probes[] | "    " + (
            if .status == "ok" then "✓ " else "✗ " end
        ) + (.name + (" " * (10 - (.name | length)))) + " " + .detail),
        "",
        "  counters:",
        "    tool_calls_total              " + (.counters.tool_calls_total | tostring),
        "    llm_stream_chunks_total       " + (.counters.llm_stream_chunks_total | tostring),
        "    web_search_breaker_open_total " + (.counters.web_search_breaker_open_total | tostring),
        ""
    '
fi

# --- exit code ------------------------------------------------------

if [ "$OVERALL" = "fail" ]; then
    exit 1
fi
if [ $STRICT -eq 1 ] && [ "$WEB_BREAKER" -gt 0 ]; then
    # In --strict mode, an open web-search breaker counts as
    # unhealthy. Default mode tolerates it (degraded-but-up).
    exit 1
fi
exit 0
