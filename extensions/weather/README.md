# Weather Extension (Rust)

Standalone Rust stdio extension for `proyecto`. Provides weather tools backed
by [Open-Meteo](https://open-meteo.com) — free, no API key required.

## Tools

- `status` — provider info, endpoints, client version, user-agent
- `current` — current conditions for a location (`location`, optional `units`)
- `forecast` — daily forecast 1–16 days (`location`, optional `days`, `units`)

Output schema documented in `skills/weather/SKILL.md`.

## Build

```bash
cargo build --release --manifest-path extensions/weather/Cargo.toml
```

The extension manifest points to `./target/release/weather` — no copy step.

## Reliability

- HTTP client: `reqwest` blocking, rustls TLS
- Connect timeout 5s, total request timeout 10s (override via `WEATHER_HTTP_TIMEOUT_SECS`)
- Retry: 3 attempts, backoff 500ms / 1s / 2s, only on 5xx and timeouts
- Circuit breaker per host, threshold 5 fails / 30s open window
- Geocoding cache, TTL 24h, LRU-ish, capacity 1000

## Tests

```bash
cargo test --manifest-path extensions/weather/Cargo.toml
```

13 tests total: 8 unit (wmo, breaker, cache) + 5 integration (mocked Open-Meteo
with `wiremock`).

## Environment variables

- `WEATHER_GEOCODING_URL` (default `https://geocoding-api.open-meteo.com/v1/search`)
- `WEATHER_FORECAST_URL` (default `https://api.open-meteo.com/v1/forecast`)
- `WEATHER_HTTP_TIMEOUT_SECS` (default `10`)

## Smoke

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"current","arguments":{"location":"Madrid"}}}' \
  | ./target/release/weather
```
