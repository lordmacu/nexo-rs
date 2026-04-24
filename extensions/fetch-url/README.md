# Fetch URL Extension (Rust)

Standalone Rust stdio extension that performs HTTP GET/POST/PUT/DELETE/HEAD/PATCH/OPTIONS
with response size cap, SSRF guard, retries, and a shared circuit breaker
(via `ext-common`). Pure Rust; depends only on `reqwest` (rustls).

## Tools

- `status` — limits, policy flags, user-agent
- `fetch_url` — `url`, optional `method`/`headers`/`body`/`max_bytes`/`timeout_secs`/`allow_private`

## Reliability

- `reqwest` blocking, rustls TLS, gzip/brotli decode
- Connect timeout 5s, total timeout 15s default (override via `timeout_secs`, max 120s)
- Retry: 3 attempts, backoff 500ms/1s/2s, on 5xx + timeouts only
- Circuit breaker: shared `ext_common::Breaker`, threshold 10 fails / 30s
- Max 5 redirects; follows `Location` within the same breaker budget
- Response cap: 5 MB default, 50 MB hard ceiling (memory-safe read)

## SSRF guard

Blocked by default:

- `localhost`, `metadata.google.internal`, `metadata`
- IPv4: loopback (127/8), private (10/8, 172.16/12, 192.168/16), link-local (169.254/16) including `169.254.169.254` metadata
- IPv6: loopback (::1), unique-local (fc00::/7), link-local (fe80::/10)

Operator can disable with `allow_private: true` per call (e.g. to hit an
internal service from the agent process).

**Note**: DNS-based SSRF is not covered — a public DNS name resolving to
127.0.0.1 after lookup will **not** be caught. Use `allow_private: true`
on trusted callers instead of trying to patch that hole piecemeal.

## Error codes

| Code | Meaning |
|------|---------|
| -32602 | Bad input (malformed URL, bad method, bad limits) |
| -32020 | Blocked private/loopback/metadata host |
| -32021 | Response size exceeded `max_bytes` (currently not returned — we truncate instead) |
| -32002 | HTTP 4xx response |
| -32003 | HTTP 5xx after retries |
| -32004 | Circuit breaker open |
| -32005 | Request timeout |

## Build & test

```bash
cargo build --release --manifest-path extensions/fetch-url/Cargo.toml
cargo test           --manifest-path extensions/fetch-url/Cargo.toml
```

17 tests: 7 unit (SSRF guards) + 10 integration (`wiremock`).

## Smoke

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fetch_url","arguments":{"url":"https://api.github.com/users/octocat"}}}' \
  | ./target/release/fetch-url
```

Output body is returned as `body_text` for text-like content-types,
`body_base64` otherwise.

## Pipeline integration

- Pair with `summarize_text` for URL-to-summary workflows
- Pair with `pdf-extract` for URL-to-PDF-text-to-summary
- Wrap both in a TaskFlow for restart-safe multi-step jobs
