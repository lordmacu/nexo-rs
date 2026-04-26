# Cost & quota controls

Operator runbook for tracking + capping LLM spend. Today the
runtime emits enough Prometheus metrics for an operator to build
their own picture; the proper `nexo costs` subcommand + budget
caps land in [Phase 45](#status).

## Estimating spend — `scripts/nexo-cost-report.sh`

Aggregates `nexo_llm_stream_chunks_total` by provider, multiplies
by a price table, prints (or emits JSON) per-provider rolling
totals.

```bash
# Human-readable report against the local /metrics endpoint
scripts/nexo-cost-report.sh

# JSON for monitoring / dashboards
scripts/nexo-cost-report.sh --json

# Custom price table (your negotiated enterprise rates)
scripts/nexo-cost-report.sh --prices ~/our-enterprise-rates.tsv

# Probe a remote daemon
scripts/nexo-cost-report.sh --metrics-host nexo.internal:9090
```

Pretty output:

```
============================================================
 nexo-rs cost report  ·  2026-04-26T15:30:00Z
============================================================

  PROVIDER                    CHUNKS     EST_TOKENS    EST_USD
  anthropic                    28391          85173    $0.7666
  minimax                       4711          14133    $0.0042
  ollama                        1208           3624    $0.0000

  total estimated: $0.7708

  disclaimer: heuristic estimate. Calibrate
    NEXO_TOKENS_PER_CHUNK once you have a measured baseline.
```

### Calibration

The default `tokens-per-chunk = 3` is a heuristic. To get an
accurate number for your deployment:

1. Find a typical conversation in transcripts (`session_logs` tool
   output).
2. Sum the `usage.total_tokens` from the `chat.completion` end
   event(s).
3. Divide by the total chunk count emitted during that
   conversation (visible in
   `nexo_llm_stream_chunks_total{provider="...",kind="text_delta"}`).
4. Set `NEXO_TOKENS_PER_CHUNK` env to the result.

Example:

```bash
# Anthropic typical: 4-token granularity per delta
NEXO_TOKENS_PER_CHUNK=4 scripts/nexo-cost-report.sh

# OpenAI typical: 1 token per delta on streaming
NEXO_TOKENS_PER_CHUNK=1 scripts/nexo-cost-report.sh
```

When the runtime ships `nexo_llm_tokens_total{provider,model,direction}`
(Phase 45 deliverable), the heuristic is replaced by direct token
counts and the calibration step disappears.

### Built-in price table

| Provider | Model | $/1M in | $/1M out |
|---|---|---|---|
| anthropic | claude-opus-4 | 15.00 | 75.00 |
| anthropic | claude-sonnet-4 | 3.00 | 15.00 |
| anthropic | claude-haiku-4 | 0.80 | 4.00 |
| openai | gpt-4o | 2.50 | 10.00 |
| openai | gpt-4o-mini | 0.15 | 0.60 |
| minimax | abab6.5s | 0.20 | 0.60 |
| minimax | M2.5 | 0.30 | 1.50 |
| gemini | gemini-1.5-pro | 1.25 | 5.00 |
| gemini | gemini-1.5-flash | 0.075 | 0.30 |
| deepseek | deepseek-chat | 0.14 | 0.28 |
| ollama | * | 0.00 | 0.00 |

These are public list prices as of 2026-04. Operators with
enterprise contracts override via `--prices`:

```tsv
provider	model	in_per_1m	out_per_1m
anthropic	claude-sonnet-4	2.40	12.00
openai	gpt-4o	2.00	8.00
```

(One row per provider×model. `*` model = applies to any model
from that provider.)

## Daily budget alerts via cron

Snapshot every 24h, mail the operator if estimated spend > cap:

```bash
# /etc/cron.daily/nexo-cost-alert
#!/bin/sh
set -eu
CAP=10.00            # $/day soft cap

REPORT=$(/opt/nexo-rs/scripts/nexo-cost-report.sh --json)
TOTAL=$(echo "$REPORT" | jq -r '.total_estimated_usd')

if awk -v t="$TOTAL" -v c="$CAP" 'BEGIN { exit !(t > c) }'; then
    echo "$REPORT" | mail -s "nexo daily spend over \$$CAP: \$$TOTAL" \
        ops@yourorg.com
fi
```

This is **alerting only**, not enforcement — the runtime keeps
serving traffic. For hard caps, wait for Phase 45.

## Hard quota caps (deferred)

Phase 45 ships per-agent monthly budget caps:

```yaml
# config/agents.yaml — once 45.x lands
agents:
  - id: kate
    cost_cap_usd:
      monthly: 50.00
      daily: 5.00
      action: refuse_new_turns   # or: warn_only, throttle
      warn_topic: alerts.kate.budget
```

When hit:
- `refuse_new_turns` — agent returns a fixed response
  ("I've reached my budget for the period; please ask the
  operator to extend.") to every new inbound. Existing in-flight
  turns finish.
- `warn_only` — log + telemetry but keep serving.
- `throttle` — switch to a cheaper model variant
  (`claude-haiku-4` instead of `claude-opus-4`) for the rest of
  the period.

Per-binding token rate limits (e.g. "WhatsApp sales binding
capped at 5k tokens/hour") layer on top of the existing
`sender_rate_limit`. Phase 45.x.

## Inspecting the metrics directly

If the script is too coarse:

```bash
# Top providers by total chunks (last 5m rate)
curl -sS http://127.0.0.1:9090/metrics | \
    awk '/^nexo_llm_stream_chunks_total/{gsub(/.*provider="/, "", $1); gsub(/".*/, "", $1); n[$1]+=$2} END{for (p in n) print n[p], p}' | \
    sort -rn

# TTFT p95 by provider (curl + jq if you have promtool):
promtool query instant http://127.0.0.1:9090 \
    'histogram_quantile(0.95, sum by (provider, le) (rate(nexo_llm_stream_ttft_seconds_bucket[5m])))'
```

The full metric inventory lives in
[Grafana dashboards → metric coverage](../ops/grafana/README.md#metric-coverage)
(but in repo as `ops/grafana/README.md`).

## Status

Tracked as [Phase 45 — Cost & quota controls](https://github.com/lordmacu/nexo-rs/blob/main/proyecto/PHASES.md#phase-45).

| Capability | Status |
|---|---|
| `scripts/nexo-cost-report.sh` heuristic estimator | ✅ shipped |
| Operator runbook (this page) | ✅ shipped |
| `nexo_llm_tokens_total{provider,model,direction}` metric | ⬜ deferred |
| Per-agent monthly budget cap (config + enforcement) | ⬜ deferred |
| `agents.<id>.cost_cap_usd` schema | ⬜ deferred |
| Per-binding token rate limit | ⬜ deferred |
| Pre-flight token-count predictor in agent prompt | ⬜ deferred |
| `nexo costs` CLI rolling 24h/7d/30d aggregator | ⬜ deferred |
| `/api/costs` admin endpoint | ⬜ deferred |
