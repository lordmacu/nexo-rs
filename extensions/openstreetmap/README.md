# OpenStreetMap Extension (Rust)

Standalone Rust stdio extension that talks to [Nominatim](https://nominatim.openstreetmap.org)
for forward and reverse geocoding. No API key required.

## Tools

- `status` — provider info, endpoint, rate-limit policy
- `search` — forward geocoding (`query`, optional `limit` 1–20, `country_codes`)
- `reverse` — reverse geocoding (`lat`, `lon`, optional `zoom` 0–18)

Schema documented in `skills/openstreetmap/SKILL.md`.

## Reliability

- HTTP client: `reqwest` blocking, rustls TLS
- Connect timeout 5s, total request timeout 10s (override via `OSM_HTTP_TIMEOUT_SECS`)
- Retry: 3 attempts, backoff 500ms / 1s / 2s, only on 5xx and timeouts
- Circuit breaker: threshold 5 fails / 30s open window
- Rate limiter: ~1 req/sec to comply with Nominatim usage policy
- User-Agent identifies the project (Nominatim policy requirement)

## Environment

- `OSM_NOMINATIM_URL` (default `https://nominatim.openstreetmap.org`)
- `OSM_HTTP_TIMEOUT_SECS` (default `10`)

## Build & test

```bash
cargo build --release --manifest-path extensions/openstreetmap/Cargo.toml
cargo test           --manifest-path extensions/openstreetmap/Cargo.toml
```

15 tests: 9 unit (breaker, cache, rate-limit) + 6 integration (`wiremock`).

## Smoke

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search","arguments":{"query":"Madrid"}}}' \
  | ./target/release/openstreetmap
```
