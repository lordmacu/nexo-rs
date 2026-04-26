# Benchmarks

The workspace ships criterion benchmark suites for every hot path
that runs on the data plane. CI executes them on every PR + weekly
on `main` so regressions are visible before merge.

## Quick run

```bash
# Single crate:
cargo bench -p nexo-resilience

# Single bench within a crate:
cargo bench -p nexo-broker --bench topic_matches

# Single group within a bench:
cargo bench -p nexo-broker --bench topic_matches -- 'topic_matches/wildcard'
```

Output goes to `target/criterion/`. Open `index.html` under that
directory in a browser for the full HTML report.

## Coverage matrix

| Crate | Bench | What it measures | Run target |
|---|---|---|---|
| `nexo-resilience` | `circuit_breaker` | `CircuitBreaker::allow` (closed + open), `on_success`, `on_failure`, 8-task concurrent allow contention | sub-100ns per call |
| `nexo-broker` | `topic_matches` | NATS-style pattern matching (exact, single-wildcard `*`, multi-wildcard `>`, 50-pattern storm) | sub-100ns per match |
| `nexo-broker` | `local_publish` | End-to-end `LocalBroker::publish` with 0 / 1 / 10 / 50 subscribers (DashMap scan + try_send + slow-consumer drop counter) | sub-10µs at 50 subs |
| `nexo-llm` | `sse_parsers` | OpenAI / Anthropic / Gemini SSE parsers, 50-chunk fixtures (typical short answer) | chunks/sec scales linearly |
| `nexo-taskflow` | `tick` | `WaitEngine::tick` at 10 / 100 / 1 000 active waiting flows | sub-millisecond at single-host scale |

## What's NOT benched yet

These are tracked under Phase 35.5 follow-up:

- `nexo-core` transcripts FTS search — needs SQLite fixture seed
  before the bench is meaningful.
- `nexo-core` redaction pipeline — wait for the local-LLM
  redaction backend (Phase 68.7) so we measure the real path
  operators ship.
- `nexo-mcp` `encode_request` / `parse_notification_method` —
  cheap to add; will land alongside an MCP-stdio round-trip
  bench.
- `nexo-memory` vector-search recall — needs a public dataset
  baseline.

Add a bench by following the patterns in `crates/<x>/benches/`:

1. `[dev-dependencies]` adds `criterion = "0.5"` (with
   `async_tokio` if you need a runtime).
2. `[[bench]]` registers `name = "<bench>"` and `harness = false`.
3. Bench file uses `Throughput::Elements(N)` so output is
   ops/sec, not raw `ns/iter`.
4. Each `criterion_group!` covers a distinct conceptual path —
   don't bundle unrelated paths.

## CI integration

`.github/workflows/bench.yml` runs the matrix on:

- every PR that touches `crates/**`, `Cargo.lock`, or
  `Cargo.toml`
- weekly on Sunday 04:00 UTC against `main`
- manual `workflow_dispatch`

Each run uploads `target/criterion/` as an artifact retained 30
days. PR runs save with `--save-baseline pr-<number>`; main runs
save as `main`. Compare locally with:

```bash
# Pull the artifact for PR #42
gh run download <run-id> --name bench-nexo-broker-<run-id>

# Compare against the local main baseline
cargo bench -p nexo-broker -- --baseline main
```

Today the CI job is **informational** — a regression doesn't
fail the PR. Once we have ~10 main runs of baseline data per
crate, the workflow gates on `>10% regression` per group. That's
Phase 35.6 done-criteria.

## Known limitations

- **GitHub Actions runners are noisy.** The `ubuntu-latest`
  shared runner tier shows ±5-10% variance on microbenchmarks.
  This is why we don't gate on small regressions yet — the
  baseline noise floor is itself ~5%.
- **Benches don't measure cold cache.** `cargo bench`'s warm-up
  phase reaches steady-state CPU caches; first-call latency on
  a cold runtime is not captured. Add a separate
  `bench_cold_*` group when this matters (it usually doesn't —
  hot path is what matters at scale).
- **No cross-crate end-to-end benchmark yet.** Phase 35.3 (load
  test rig) covers that; today's suites are per-crate
  microbenchmarks.

## Reading criterion output

A typical run prints:

```
publish/mixed_50_subs   time:   [12.347 µs 12.451 µs 12.567 µs]
                        thrpt:  [3.9786 Melem/s 4.0153 Melem/s 4.0494 Melem/s]
                 change: time:   [-0.4%  +0.3%  +1.1%]    (p = 0.62 > 0.05)
                         thrpt:  [-1.1% -0.3% +0.4%]
                         No change in performance detected.
```

- `time` is the per-iteration latency (lower better).
- `thrpt` is throughput (higher better) — only present when the
  bench declared `Throughput::Elements(N)`.
- `change` compares against the previous run on the same
  hardware. `p > 0.05` means the difference is within noise.

Look for `change` reporting "Performance has regressed" with a
red bar — that's the signal a PR introduced a regression.
