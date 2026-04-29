## Plan: 77.3 — sessionMemoryCompact + postCompactCleanup

### Crate(s) afectados
- `crates/driver-types/` — `CompactSummaryStore` trait + `CompactSummary` struct
- `crates/driver-loop/` — orchestrator wiring + `SqliteCompactSummaryStore` + cleanup
- `crates/core/` — `TranscriptLine::CompactBoundary` variant
- `docs/` — actualizar `compact-tiers.md`

### Archivos nuevos
- `crates/driver-loop/src/compact_store.rs` — `SqliteCompactSummaryStore` impl
- `crates/driver-loop/src/post_compact_cleanup.rs` — `PostCompactCleanup`

### Archivos modificados
- `crates/driver-types/src/compact_policy.rs` — `CompactSummaryStore` trait + `CompactSummary` struct + `SmCompactConfig`
- `crates/driver-loop/src/events.rs` — `CompactSummaryStored` variant
- `crates/driver-loop/src/orchestrator.rs` — wire store + persist on CompactCompleted + inject on resume
- `crates/driver-loop/src/config.rs` — `SmCompactConfig` en `CompactPolicyConfig.auto`
- `crates/driver-loop/src/lib.rs` — registrar nuevos modulos
- `crates/core/src/agent/transcripts.rs` — `TranscriptLine::CompactBoundary`
- `docs/src/ops/compact-tiers.md` — documentar tier 3

### Pasos

#### 1. [ ] `CompactSummaryStore` trait + types — `crates/driver-types/src/compact_policy.rs`
Agregar al final del archivo:
- `CompactSummary` struct: `agent_id`, `summary`, `turn_index`, `before_tokens`, `after_tokens`, `stored_at`
- `CompactSummaryStore` trait: `store()` + `load()`
- Re-export desde `lib.rs`
Done: `cargo build -p nexo-driver-types` compila.

#### 2. [ ] `SqliteCompactSummaryStore` — `crates/driver-loop/src/compact_store.rs`
- Struct con `pool: SqlitePool` (o `Arc<LongTermMemory>`)
- `impl CompactSummaryStore for SqliteCompactSummaryStore`
- `store()`: usa `long_term.remember(agent_id, json, &["compact_summary"])`
- `load()`: usa `long_term.recall(agent_id, "compact_summary")`, toma el mas reciente
- `NoopCompactSummaryStore` para tests
Done: `cargo build -p nexo-driver-loop` compila.

#### 3. [ ] Wire store en orchestrator — `crates/driver-loop/src/orchestrator.rs`
- Agregar `compact_store: Arc<dyn CompactSummaryStore>` al `DriverOrchestrator`
- Builder: `.compact_store(s)` setter, default `NoopCompactSummaryStore`
- En `run_goal`, despues de `CompactCompleted` + `record_success`: extraer summary de `result.final_text`, llamar `store()`, emitir `CompactSummaryStored`
- Antes del loop principal: `load(goal_id)` → si `Some`, inyectar `compact_summary` en `next_extras`
Done: `cargo build -p nexo-driver-loop` compila.

#### 4. [ ] `CompactSummaryStored` event — `crates/driver-loop/src/events.rs`
Agregar variant:
```rust
CompactSummaryStored {
    goal_id: GoalId,
    turn_index: u32,
    before_tokens: u64,
    after_tokens: u64,
},
```
NATS subject: `agent.driver.compact.summary_stored`
Done: compila.

#### 5. [ ] `PostCompactCleanup` — `crates/driver-loop/src/post_compact_cleanup.rs`
- Struct vacio con metodo `run(&self)` (o free function)
- Limpia: nada por ahora (77.1 microcompact state es interno a driver-claude, classifier approvals cache no existe aun). Placeholder para 77.5+.
- Llamado desde orchestrator despues de persistir summary.
Done: compila.

#### 6. [ ] `TranscriptLine::CompactBoundary` — `crates/core/src/agent/transcripts.rs`
Agregar variant:
```rust
CompactBoundary {
    uuid: String,
    token_count: u64,
    turn_index: u32,
}
```
Actualizar `write_jsonl` / serialization si necesario.
Done: `cargo build -p nexo-core` compila.

#### 7. [ ] Wire config — `crates/driver-loop/src/config.rs`
Agregar `SmCompactConfig` dentro de `CompactPolicyConfig`:
```rust
pub struct SmCompactConfig {
    pub min_tokens: u64,       // default 10_000
    pub max_tokens: u64,       // default 40_000
    pub store_in_long_term_memory: bool, // default true
}
```
Campo `sm_compact: Option<SmCompactConfig>` en `CompactPolicyConfig`.
Done: compila.

#### 8. [ ] Tests
- `compact_store.rs`: test `store_and_load_roundtrip` (con SQLite in-memory)
- `orchestrator.rs` tests existentes no se rompen (noop store default)
Done: `cargo test -p nexo-driver-loop` pasa.

#### 9. [ ] Docs sync — `docs/src/ops/compact-tiers.md`
- Agregar Tier 3: session memory compact
- YAML de ejemplo con `sm_compact`
- Evento `CompactSummaryStored`
Done: `mdbook build docs` limpio.

### Tests a escribir

| # | Test | Ubicacion | Verifica |
|---|------|-----------|----------|
| 1 | `store_and_load_roundtrip` | `driver-loop/src/compact_store.rs` | Store → load devuelve mismo summary |
| 2 | `load_returns_none_for_unknown_goal` | `driver-loop/src/compact_store.rs` | Goal sin summary → None |
| 3 | `noop_store_always_ok` | `driver-loop/src/compact_store.rs` | Noop no falla |

### Riesgos

- **SqlitePool en driver-loop** — driver-loop ya depende de `sqlx`. `SqliteCompactSummaryStore` puede usar un pool separado o compartir el de `nexo_memory`. **Mitigacion:** recibe `Arc<LongTermMemory>`, no un pool directo.
- **Summary injection en resume** — el `final_text` del compact turn puede ser grande. **Mitigacion:** truncar a `max_tokens` chars (~160K chars para 40K tokens).
