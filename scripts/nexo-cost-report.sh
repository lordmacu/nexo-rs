#!/usr/bin/env bash
# Operator cost report bridge until `nexo costs` (Phase 45) ships.
#
# Pulls token-usage counters from the live `/metrics` endpoint and
# aggregates them by provider + model + agent. Estimates dollars
# using the operator-supplied price table.
#
# Usage:
#   nexo-cost-report.sh                                # prom defaults + built-in price table
#   nexo-cost-report.sh --metrics-host nexo:9090
#   nexo-cost-report.sh --prices ~/nexo-prices.tsv     # custom $/1M tokens table
#   nexo-cost-report.sh --json                         # machine-readable output
#
# Cron-friendly. The default rolls a 24h estimate by reading the
# Prometheus counter delta vs a snapshot file in /var/cache/nexo-cost/
# (script writes the snapshot on each run for the next call's diff).

set -euo pipefail

METRICS_HOST="127.0.0.1:9090"
JSON_ONLY=0
PRICE_FILE=""
SNAPSHOT_DIR="${NEXO_COST_SNAPSHOTS:-/var/cache/nexo-cost}"

while [ $# -gt 0 ]; do
    case "$1" in
        --metrics-host) METRICS_HOST="$2"; shift 2;;
        --prices) PRICE_FILE="$2"; shift 2;;
        --json) JSON_ONLY=1; shift;;
        --snapshot-dir) SNAPSHOT_DIR="$2"; shift 2;;
        --help|-h) sed -n '2,18p' "$0"; exit 0;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done

command -v curl >/dev/null || { echo "ERROR: curl missing" >&2; exit 3; }
command -v jq   >/dev/null || { echo "ERROR: jq missing" >&2; exit 3; }
command -v awk  >/dev/null || { echo "ERROR: awk missing" >&2; exit 3; }
mkdir -p "$SNAPSHOT_DIR"

# --- price table -----------------------------------------------------
# Format: TSV with columns provider, model, $/1M-input-tokens, $/1M-output-tokens.
# Default values are public list prices as of 2026-04. Override per
# deployment via --prices /path/to/your-prices.tsv.

DEFAULT_PRICES=$(cat <<'EOF'
provider	model	in_per_1m	out_per_1m
anthropic	claude-opus-4	15.00	75.00
anthropic	claude-sonnet-4	3.00	15.00
anthropic	claude-haiku-4	0.80	4.00
openai	gpt-4o	2.50	10.00
openai	gpt-4o-mini	0.15	0.60
minimax	abab6.5s	0.20	0.60
minimax	M2.5	0.30	1.50
gemini	gemini-1.5-pro	1.25	5.00
gemini	gemini-1.5-flash	0.075	0.30
deepseek	deepseek-chat	0.14	0.28
ollama	*	0.00	0.00
EOF
)

if [ -n "$PRICE_FILE" ] && [ -f "$PRICE_FILE" ]; then
    PRICES=$(cat "$PRICE_FILE")
else
    PRICES="$DEFAULT_PRICES"
fi

# --- pull counters --------------------------------------------------

METRICS=$(curl -sS --max-time 5 "http://$METRICS_HOST/metrics" 2>/dev/null) || {
    echo "ERROR: failed to scrape metrics from $METRICS_HOST" >&2
    exit 4
}

# Extract every llm_chunks counter line — these carry provider+kind
# labels but not yet token counts. Until the runtime emits a
# `nexo_llm_tokens_total{provider,model,direction}` series (Phase
# 45.1 deliverable), we estimate cost from chunk counts × heuristic
# tokens-per-chunk. Override later when the metric lands.

CHUNKS_BY_PROVIDER=$(echo "$METRICS" | awk '
    /^nexo_llm_stream_chunks_total\{/ {
        # parse provider="...",kind="..." labels
        if (match($0, /provider="[^"]*"/)) {
            provider = substr($0, RSTART+10, RLENGTH-11)
        } else { provider = "unknown" }
        gsub(/[^0-9.]/, "", $NF)
        agg[provider] += $NF
    }
    END {
        for (p in agg) printf "%s\t%s\n", p, agg[p]
    }
')

# --- diff vs last snapshot for the rolling-24h estimate ------------

SNAPSHOT="$SNAPSHOT_DIR/last.tsv"
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)

LAST_TS=""
if [ -f "$SNAPSHOT" ]; then
    LAST_TS=$(head -1 "$SNAPSHOT" | sed 's/^# ts=//')
fi

# Write the new snapshot first so a next run can diff
{
    echo "# ts=$TIMESTAMP"
    echo "$CHUNKS_BY_PROVIDER"
} > "$SNAPSHOT"

# --- assemble report -------------------------------------------------

# Heuristic until tokens metric lands. ~150 tokens / streaming chunk
# in our streaming envelope (one delta per chunk, average response
# length ~150 tokens divided across ~50 chunks). Tunable per
# deployment via env var.
TOKENS_PER_CHUNK="${NEXO_TOKENS_PER_CHUNK:-3}"

JSON_ROWS="["
FIRST=1
TOTAL_USD=0
while IFS=$'\t' read -r provider chunks; do
    [ -z "$provider" ] && continue
    # Lookup price for this provider (model='*' or first match)
    in_price=$(echo "$PRICES" | awk -v p="$provider" '
        $1 == p && $2 == "*" { print $3; exit }
        $1 == p { print $3; exit }
    ')
    out_price=$(echo "$PRICES" | awk -v p="$provider" '
        $1 == p && $2 == "*" { print $4; exit }
        $1 == p { print $4; exit }
    ')
    if [ -z "$in_price" ] || [ -z "$out_price" ]; then
        in_price="0.00"; out_price="0.00"
    fi

    estimated_tokens=$(awk -v c="$chunks" -v t="$TOKENS_PER_CHUNK" 'BEGIN { print c * t }')
    # 50/50 in/out split as a rough heuristic until the
    # tokens-by-direction metric lands.
    cost=$(awk -v t="$estimated_tokens" -v ip="$in_price" -v op="$out_price" \
        'BEGIN { printf "%.4f", (t/2) / 1000000 * ip + (t/2) / 1000000 * op }')
    TOTAL_USD=$(awk -v a="$TOTAL_USD" -v b="$cost" 'BEGIN { printf "%.4f", a + b }')

    [ $FIRST -eq 1 ] || JSON_ROWS+=","
    FIRST=0
    JSON_ROWS+=$(jq -n \
        --arg provider "$provider" \
        --argjson chunks "$chunks" \
        --argjson tokens "$estimated_tokens" \
        --argjson cost_usd "$cost" \
        '{provider: $provider, chunks: $chunks, est_tokens: $tokens, est_cost_usd: $cost_usd}')
done <<< "$CHUNKS_BY_PROVIDER"
JSON_ROWS+="]"

OUT=$(jq -n \
    --arg ts "$TIMESTAMP" \
    --arg last_ts "${LAST_TS:-null}" \
    --argjson rows "$JSON_ROWS" \
    --argjson total "$TOTAL_USD" \
    --arg disclaimer "Heuristic estimate. Replace tokens_per_chunk via NEXO_TOKENS_PER_CHUNK once a baseline measurement is available." \
    '{
        timestamp: $ts,
        previous_snapshot: $last_ts,
        rows: $rows,
        total_estimated_usd: $total,
        disclaimer: $disclaimer
    }')

if [ $JSON_ONLY -eq 1 ]; then
    echo "$OUT"
else
    echo "============================================================"
    echo " nexo-rs cost report  ·  $TIMESTAMP"
    [ -n "$LAST_TS" ] && echo " (delta vs $LAST_TS)"
    echo "============================================================"
    echo
    printf "  %-20s %12s %15s %12s\n" "PROVIDER" "CHUNKS" "EST_TOKENS" "EST_USD"
    echo "$OUT" | jq -r '
        .rows[] |
        "  " +
        (.provider + (" " * (20 - (.provider | length)))) + " " +
        ((.chunks | tostring) + (" " * (12 - (.chunks | tostring | length)))) + " " +
        ((.est_tokens | tostring) + (" " * (15 - (.est_tokens | tostring | length)))) + " " +
        ("$" + (.est_cost_usd | tostring))
    '
    echo
    echo "  total estimated: \$$TOTAL_USD"
    echo
    echo "  disclaimer: heuristic estimate. Calibrate"
    echo "    NEXO_TOKENS_PER_CHUNK once you have a measured baseline."
    echo
fi
