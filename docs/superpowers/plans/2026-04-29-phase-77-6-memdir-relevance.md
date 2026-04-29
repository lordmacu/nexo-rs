## Plan: 77.6 — memdir findRelevantMemories + memoryAge decay

### Crate(s) afectados
- `crates/memory/` — core changes (MemoryType, DB migration, scoring, find_relevant)
- `crates/core/` — `already_surfaced` field in Session

### Archivos nuevos
- `crates/memory/src/relevance.rs` — `MemoryType`, `ScoredMemory`, `score_memories()`, `freshness_note()`, `find_relevant()`
  Separate module for relevance scoring — keeps `long_term.rs` from growing beyond its ~1800 lines.

### Archivos modificados
- `crates/memory/src/long_term.rs` — DB migration (memory_type column), `MemoryEntry.memory_type` field, `remember()` stores memory_type, hydrate paths read it, `aggregate_signals()` per-type half-life
- `crates/memory/src/lib.rs` — re-exports from `relevance` module
- `crates/core/src/session/types.rs` — `already_surfaced: HashSet<Uuid>` field in `Session`

### Pasos

1. [ ] **MemoryType enum + memory_type field in MemoryEntry** — `crates/memory/src/relevance.rs` — `MemoryType` enum (User/Feedback/Project/Reference) with `half_life_days()` and `parse()`, `MemoryEntry.memory_type: Option<MemoryType>` field, done when `cargo build -p nexo-memory` compiles clean

2. [ ] **DB migration for memory_type column** — `crates/memory/src/long_term.rs` — `ALTER TABLE memories ADD COLUMN memory_type TEXT` idempotent via `is_duplicate_column_error()`, `remember()` stores `memory_type` (passed as param or default None), all hydration paths (FTS recall_with_tags, vector recall_vector, recall_hybrid) read `memory_type` from row, done when migration runs without error and existing tests pass

3. [ ] **ScoredMemory + score_memories()** — `crates/memory/src/relevance.rs` — `ScoredMemory { entry, score, freshness_warning }`, `score_memories(entries, query_embedding, now) -> Vec<(f32, MemoryEntry)>` composite scoring: `similarity × recency(per-type half-life) × log1p(frequency)`, NaN/inf guard → 0.0, half-life=0 guard → recency=0.0, future mtime clamp → 0 days, done when unit tests pass for scoring math

4. [ ] **freshness_note() staleness caveat** — `crates/memory/src/relevance.rs` — `freshness_note(entry, now, threshold_days) -> Option<String>`, returns `<system-reminder>` block when memory age > threshold, done when unit test verifies format and threshold behavior

5. [ ] **find_relevant() unified entry point** — `crates/memory/src/relevance.rs` — `find_relevant(ltm, agent_id, query, limit, already_surfaced, now) -> Vec<ScoredMemory>`, wraps `recall_hybrid()` + `score_memories()` + filter `already_surfaced` + top-N truncation, done when integration test exercises full pipeline

6. [ ] **Per-type half-life in aggregate_signals()** — `crates/memory/src/long_term.rs` — Replace hardcoded `7.0` days with `MemoryType::half_life_days()`, pass `memory_type` (or default Project) to function, done when existing recall signals tests pass with per-type decay

7. [ ] **already_surfaced in Session** — `crates/core/src/session/types.rs` — Add `already_surfaced: HashSet<Uuid>` field, `mark_surfaced(id)` and `is_surfaced(id)` helper methods, done when Session tests compile and pass

8. [ ] **Re-exports + lib.rs wiring** — `crates/memory/src/lib.rs` — `pub mod relevance;` + `pub use relevance::{MemoryType, ScoredMemory, score_memories, freshness_note, find_relevant};`, done when `cargo build --workspace` passes

9. [ ] **Tests + docs sync** — Unit tests in `relevance.rs` for edge cases (NaN, clock skew, zero half-life, empty candidates, all surfaced, legacy None type), update `PHASES.md` 77.6 → ✅, update CLAUDE.md counter 236→237, done when `cargo test --workspace` passes and counters updated

### Tests a escribir
- `tests/relevance.rs` (inside `#[cfg(test)] mod tests` in `relevance.rs`):
  - `score_memories_returns_sorted_by_composite` — 3 entries with different ages/types → scores in descending order
  - `score_memories_nan_cosine_guarded` — NaN similarity → score 0.0
  - `score_memories_zero_half_life` — half_life=0 → recency=0.0, score=0.0
  - `score_memories_future_mtime` — clock skew → 0 days age, recency=1.0
  - `score_memories_legacy_none_type` — None → treated as Project (90d half-life)
  - `freshness_note_below_threshold` — 0.5d old → None
  - `freshness_note_above_threshold` — 2d old → Some(warning)
  - `freshness_note_threshold_zero` — threshold=0 → always warns
  - `find_relevant_filters_surfaced` — 3 candidates, 2 already_surfaced → returns 1
  - `find_relevant_empty_candidates` — empty → returns []

### Riesgos
- **`memory_type` NULL in existing rows**: handled by `Option<MemoryType>` + default `Project` in scoring. No data migration needed.
- **`aggregate_signals()` signature change**: only used internally in `long_term.rs` — call sites in `recall_signals_for_agent` are local, low blast radius.
- **Session field addition**: `already_surfaced` is only used by the relevance path, not persisted. Session is an in-memory struct — no schema change.
