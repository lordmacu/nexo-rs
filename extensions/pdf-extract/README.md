# PDF Extract Extension (Rust)

Standalone Rust stdio extension that extracts plain UTF-8 text from local PDF
files. Pure Rust — no `pdftotext` / Poppler / Python binaries required. Built
on the [`pdf-extract`](https://crates.io/crates/pdf-extract) crate.

## Tools

- `status` — provider info, file-size limit, default char cap
- `extract_text` — `path` (≤ 25 MB PDF) + optional `max_chars` (default 200 000)

## Reliability

- File size hard limit: 25 MB (configurable in `src/tools.rs:MAX_FILE_BYTES`)
- Default char cap: 200 000 — fits within a typical LLM context window
- Typed errors:
  - `-32602` bad input (missing file, empty path, bad max_chars)
  - `-32006` provider failure (malformed PDF, unsupported encoding)

## Build & test

```bash
cargo build --release --manifest-path extensions/pdf-extract/Cargo.toml
cargo test           --manifest-path extensions/pdf-extract/Cargo.toml
```

8 integration tests (status + extraction happy path + truncate + 4 validation
+ non-pdf → provider error). Fixture: `tests/fixtures/hello.pdf` (2.4 KB,
generated with `ps2pdf`).

## Smoke

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"extract_text","arguments":{"path":"/tmp/note.pdf","max_chars":50000}}}' \
  | ./target/release/pdf-extract
```

Output:

```json
{
  "bytes": 2440,
  "char_count": 16,
  "max_chars": 200000,
  "path": "tests/fixtures/hello.pdf",
  "text": "Hola mundo PDF",
  "total_char_count": 16,
  "truncated": false
}
```

## Pipeline integration

Typical use: chain with `summarize_text` when the user drops a PDF.

1. `pdf-extract.extract_text { path: "/tmp/doc.pdf", max_chars: 50000 }` → text
2. `summarize.summarize_text { text: <output>, length: "medium" }` → summary
