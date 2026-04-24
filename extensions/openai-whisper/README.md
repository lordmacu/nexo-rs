# OpenAI Whisper Extension (Rust)

Standalone Rust stdio extension that transcribes local audio files via any
OpenAI-compatible `/audio/transcriptions` endpoint (OpenAI, Groq Whisper-large-v3,
local whisper.cpp HTTP server, etc.). No `curl` subprocess; multipart upload via
`reqwest`.

## Tools

- `status` — endpoint, default model, key presence, file-size limit
- `transcribe_file` — `file_path` (≤ 25 MB) + optional `model`, `language`, `prompt`, `response_format`, `temperature`

## Reliability

- `reqwest` blocking + multipart, rustls TLS
- Connect timeout 5s, total request timeout 120s (override via `WHISPER_HTTP_TIMEOUT_SECS`)
- Retry: 2 attempts, backoff 1500ms, only on 5xx and timeouts (audio uploads are expensive — not aggressive)
- Circuit breaker: 5 fails / 60s open
- Typed errors: `Unauthorized` (-32011), `PayloadTooLarge` (-32014), `UnsupportedMedia` (-32015), `EmptyTranscript` (-32007)

## Environment

- `WHISPER_OPENAI_API_KEY` (required)
- `WHISPER_OPENAI_URL` (default `https://api.openai.com/v1`)
- `WHISPER_MODEL` (default `whisper-1`; override per request via `model` arg)
- `WHISPER_HTTP_TIMEOUT_SECS` (default `120`)

To use Groq Whisper-large-v3:
```bash
WHISPER_OPENAI_URL=https://api.groq.com/openai/v1
WHISPER_OPENAI_API_KEY=$GROQ_API_KEY
WHISPER_MODEL=whisper-large-v3
```

## Build & test

```bash
cargo build --release --manifest-path extensions/openai-whisper/Cargo.toml
cargo test           --manifest-path extensions/openai-whisper/Cargo.toml
```

10 integration tests via `wiremock` (status, text/json formats, 401/413/415/5xx, validation).

## Smoke

```bash
WHISPER_OPENAI_API_KEY=sk-xxx \
  echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"transcribe_file","arguments":{"file_path":"/tmp/voice.mp3","language":"es","response_format":"verbose_json"}}}' \
  | ./target/release/openai-whisper
```
