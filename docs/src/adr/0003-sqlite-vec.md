# ADR 0003 — sqlite-vec for vector search

**Status:** Accepted
**Date:** 2026-02

## Context

Agents benefit from semantic recall — surface a memory whose text
doesn't share keywords with the query but shares meaning. The usual
playbook: run a dedicated vector database.

Requirements:

- Zero extra infrastructure for single-machine deployments
- Same durability and transactional model as the rest of memory
- Embedding-dimension sanity checks at startup
- Hybrid retrieval (keyword ⊔ vector) without a separate query plane

Alternatives considered:

- **Qdrant / Weaviate / Milvus** — all excellent; all require an
  extra service, network hop, and ops surface
- **pgvector** — would force Postgres everywhere, abandoning SQLite
  for long-term memory
- **Simple numpy file + linear scan** — works for small datasets,
  falls over past ~10k memories per agent

## Decision

Use **sqlite-vec**: a SQLite extension that adds a `vec0` virtual
table in the **same DB file** as long-term memory.

- One SQLite file holds `memories`, `memories_fts`, and
  `vec_memories` — a single `JOIN` returns content + tags alongside
  similarity
- Dimension is checked at schema init; mismatch between config and
  existing rows aborts startup with an explicit message
- `sqlite3_auto_extension` registers once per process
- Hybrid retrieval uses Reciprocal Rank Fusion (K=60) over the
  keyword FTS5 hits and the vector neighbors

## Consequences

**Positive**

- Zero-infra single-machine deploys keep working — no extra
  service to run
- Backups, replication, export are all just "copy the `.db` file"
- Transactional writes: `INSERT` into `memories` + `vec_memories`
  in one statement; no dual-write races
- Hybrid retrieval is easy (see [vector docs](../memory/vector.md))

**Negative**

- sqlite-vec is newer than Qdrant; its indexing algorithm improves
  over time. Large indexes may need re-sorting periodically
- Changing embedding models (even same-dimension ones) produces a
  stale index — the ADR doesn't solve this, users must reindex
- The `sqlite3_auto_extension` registration happens once per process
  and has caught test suites that spawn many short-lived connections
  off-guard

**Swap-out path**

`EmbeddingProvider` is a trait and the `recall_mode = vector` branch
is a single code path. Replacing sqlite-vec with Qdrant is a
day's work, not a rewrite.
