# Summarize Extension (Rust)

Standalone Rust stdio extension for `proyecto`. Summarizes plain text and local
UTF-8 files via any OpenAI-compatible `/chat/completions` endpoint (OpenAI,
MiniMax, Groq, llama.cpp server, etc.). No `curl` subprocess.

## Tools

- `status` — endpoint, model, key presence, limits
- `summarize_text` — `text` + optional `length` (short|medium|long, default medium) + optional `language`
- `summarize_file` — `path` to UTF-8 file (max 1 MB / 60k chars) + same options

## Reliability

- `reqwest` blocking, rustls TLS
- Connect timeout 5s, total request timeout 30s (override via `SUMMARIZE_HTTP_TIMEOUT_SECS`)
- Retry: 3 attempts, backoff 500ms / 1s / 2s, only on 5xx and timeouts
- Circuit breaker: 5 fails / 30s open
- Typed errors: `Unauthorized` (-32011), `Forbidden` (-32012), `EmptyCompletion` (-32007)

## Environment

- `SUMMARIZE_OPENAI_API_KEY` (required)
- `SUMMARIZE_OPENAI_URL` (default `https://api.openai.com/v1`)
- `SUMMARIZE_MODEL` (default `gpt-4o-mini`)
- `SUMMARIZE_HTTP_TIMEOUT_SECS` (default `30`)

To use MiniMax:
```bash
SUMMARIZE_OPENAI_URL=https://api.minimax.chat/v1
SUMMARIZE_OPENAI_API_KEY=$MINIMAX_API_KEY
SUMMARIZE_MODEL=MiniMax-M2.5
```

## Build & test

```bash
cargo build --release --manifest-path extensions/summarize/Cargo.toml
cargo test           --manifest-path extensions/summarize/Cargo.toml
```

10 tests: 4 unit (breaker) + 6 integration (`wiremock`).

## Smoke

```bash
SUMMARIZE_OPENAI_API_KEY=sk-xxx \
  echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"summarize_text","arguments":{"text":"...","length":"short"}}}' \
  | ./target/release/summarize
```
