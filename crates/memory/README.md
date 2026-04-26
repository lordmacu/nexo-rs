# nexo-memory

> Three-tier memory for Nexo agents — short-term (in-process LRU), long-term (SQLite), and vector (sqlite-vec for semantic recall). Single SQLite file, zero extra infrastructure.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`ShortTerm`** — in-process per-session ring buffer of
  recent messages. Cheap (no I/O); the LLM sees this as the
  conversation context in every prompt.
- **`LongTermMemory`** — SQLite-backed. `store(text, tags, ts)`,
  `recall(query, k)`, `purge_older_than(...)`. Tag indexing
  for filtered recall.
- **`vector` module** — sqlite-vec integration for semantic
  similarity. `pack_f32` / `unpack_f32` helpers (MSRV-safe;
  tested on Rust 1.80) for the embedding payload.
- **Single SQLite file** — `data/memory.db` holds all three
  tiers. Backups + restores are file-level (see
  `scripts/nexo-backup.sh`).
- **No external service** — no Pinecone, no Qdrant, no Redis.
  Sub-millisecond reads at single-host scale; switch to
  sharded sqlite-vec or an external vector DB only when
  the workload genuinely demands it.

## Public API

```rust
pub struct LongTermMemory { /* … */ }

impl LongTermMemory {
    pub async fn open(path: &Path) -> Result<Self>;
    pub async fn store(&self, text: &str, tags: &[&str], embedding: Option<Vec<f32>>) -> Result<i64>;
    pub async fn recall(&self, query: &str, k: usize) -> Result<Vec<MemoryHit>>;
    pub async fn recall_by_tag(&self, tag: &str, k: usize) -> Result<Vec<MemoryHit>>;
    pub async fn purge_older_than(&self, ts: DateTime<Utc>) -> Result<u64>;
}

pub fn pack_f32(values: &[f32]) -> Vec<u8>;
pub fn unpack_f32(bytes: &[u8]) -> Option<Vec<f32>>;
```

## Configuration

```yaml
# config/memory.yaml
memory:
  short_term:
    capacity_per_session: 50
  long_term:
    sqlite:
      path: data/memory.db
  vector:
    enabled: true
    dimensions: 1536
```

## Install

```toml
[dependencies]
nexo-memory = "0.1"
```

## Documentation for this crate

- [Short-term memory](https://lordmacu.github.io/nexo-rs/memory/short-term.html)
- [Long-term memory](https://lordmacu.github.io/nexo-rs/memory/long-term.html)
- [Vector search](https://lordmacu.github.io/nexo-rs/memory/vector.html)
- [sqlite-vec ADR](https://lordmacu.github.io/nexo-rs/adr/0003-sqlite-vec.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
