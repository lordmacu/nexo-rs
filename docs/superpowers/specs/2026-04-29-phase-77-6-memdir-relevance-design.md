## Spec: 77.6 — memdir findRelevantMemories + memoryAge decay

### Descripción
Scoring compuesto para memorias recuperadas: `similarity × recency(per-type half-life) × log1p(frequency)`, con anotación de staleness. Unifica `recall_hybrid()` + ranking + filtro `already_surfaced` + caveat de antigüedad en un solo entry point `find_relevant()`.

### Alcance (IN)
- `MemoryType` enum: `User`, `Feedback`, `Project`, `Reference` con `half_life_days()` asociado
- Columna `memory_type TEXT` en tabla `memories` (migración idempotente)
- `MemoryEntry.memory_type: Option<MemoryType>` — `None` = legacy, tratado como `Project` (half-life 90d, el más conservador)
- `score_memories(entries, query_embedding, now) -> Vec<(f32, MemoryEntry)>` — scoring compuesto
- `freshness_note(entry, now) -> Option<String>` — `<system-reminder>` para memorias > umbral
- `find_relevant(agent_id, query, limit, already_surfaced, now) -> Vec<ScoredMemory>` — entry point unificado
- `ScoredMemory` struct: `entry: MemoryEntry`, `score: f32`, `freshness_warning: Option<String>`
- `already_surfaced: HashSet<Uuid>` tracking en `SessionContext`
- `aggregate_signals()` usa `half_life_days` del `MemoryType` en vez del hardcode `7.0`

### Fuera de alcance (OUT)
- Selección por LLM (side-query como Claude Code) — requiere llamada extra, se evaluará en 77.6.b
- `recentTools` filter — no tenemos tool-usage tracking en el path de recall
- `MEMORY.md` truncation (200 lines / 25 KB) — ya lo maneja `memdir.rs` en otro scope
- Persistencia de `already_surfaced` entre reinicios de daemon — se limpia al perder la sesión
- Mutación de `memory_type` en runtime — solo se setea al escribir (`remember()` / `insert()`), no se actualiza

### Interfaces

#### Trait / Struct

```rust
/// Four-type taxonomy matching Phase 77.5 extractMemories.
/// Half-life rationale:
///   user      — ∞ (preferencias no caducan)
///   feedback  — 365 d (correcciones de approach envejecen lento)
///   project   —  90 d (contexto de proyecto rota rápido)
///   reference — ∞ (pointers externos no caducan)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryType {
    /// Half-life in days. Large sentinel (10_000 d ≈ 27 years) instead of
    /// f64::MAX to keep exp() numerically stable. Callers treat ≥ 10_000
    /// as "effectively infinite" — recency ≈ 1.0 always.
    pub fn half_life_days(self) -> f64 {
        match self {
            MemoryType::User => 10_000.0,
            MemoryType::Feedback => 365.0,
            MemoryType::Project => 90.0,
            MemoryType::Reference => 10_000.0,
        }
    }

    /// Lenient parser. Unknown values → None (not error).
    /// Legacy memories without the column also map to None.
    pub fn parse(s: &str) -> Option<Self> { /* ... */ }
}

/// A memory entry with its composite score and optional staleness warning.
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    pub entry: MemoryEntry,
    /// Composite score: similarity × recency × log1p(frequency). [0, 1].
    pub score: f32,
    /// `<system-reminder>` block when the memory is older than the
    /// freshness threshold, else None.
    pub freshness_warning: Option<String>,
}

impl MemoryEntry {
    /// Add memory_type field. Default None for backward compatibility.
    /// Serialized as Option<String> in DB (NULL or snake_case text).
    pub memory_type: Option<MemoryType>,
}
```

#### Config (YAML)

```yaml
# New block: agent.yaml or binding-level memory config
memory:
  relevance:
    # Half-life overrides per type (days). Defaults match MemoryType::half_life_days().
    # Set to 0 to disable decay for that type entirely.
    half_life_days:
      user: 10000       # effectively infinite
      feedback: 365
      project: 90
      reference: 10000  # effectively infinite
    # Memories older than this (in days) get a staleness caveat.
    # Default 1. Set to 0 to warn on all memories. Set to i32::MAX to disable.
    freshness_warning_days: 1
    # Max memories returned by find_relevant(). Default 5.
    max_relevant: 5
```

No nuevos topics/events — `find_relevant()` es sincrónico dentro del turno, no emite eventos.

### Casos de uso

1. **Happy path:** `find_relevant("agent-1", "deploy process", 5, &surfaced, now)` → `recall_hybrid()` trae 12 candidatos, `score_memories()` rankea por `cosine × recency × log1p(freq)`, filtra 2 ya surfaced, retorna top 3 con scores y freshness warnings para memorias >1 día.

2. **Empty candidate set:** `recall_hybrid()` retorna `[]` → `find_relevant()` retorna `Vec::new()` sin error.

3. **All candidates already surfaced:** 5 candidatos, los 5 en `already_surfaced` → retorna `[]`. No fuerza resultados repetidos.

4. **Vector embedding fails:** `recall_vector()` error → fallback a FTS-only con scoring por recency + frequency (sin componente cosine) — score máximo 0.5 sin similitud semántica.

5. **Legacy memory sin memory_type:** `memory_type = None` → tratado como `Project` (half-life 90d, el default más conservador). No rompe.

6. **Clock skew — mtime futuro:** `memoryAgeDays` retorna `0` (max(0, floor(...))) como en `claude-code-leak/src/memdir/memoryAge.ts`. No decae artificialmente.

7. **half_life_days = 0:** División por cero en `exp(-days * ln2 / 0.0)` → detectado y tratado como `recency = 0.0` (decaimiento instantáneo). No panic.

8. **NaN en cosine similarity:** `score_memories()` usa `score.is_finite()` guard — valores no finitos se reemplazan con `0.0`.

9. **Concurrent `already_surfaced` mutation:** `SessionContext` protege con `Mutex<HashSet<Uuid>>`. El recall es read-only sobre el set.

10. **DB migration en BD existente:** `ALTER TABLE memories ADD COLUMN memory_type TEXT` — idempotente via `is_duplicate_column_error()`. Mismo patrón que `concept_tags`.

### Dependencias
- Crates nuevos: ninguno
- Crates del workspace: `nexo-memory` (modificar), `nexo-core` (modificar `SessionContext`)
- No nuevos deps externos

### Decisiones de diseño
- **Scoring compuesto, no LLM** — Claude Code usa Sonnet side-query para seleccionar memorias. Nosotros usamos fórmula numérica porque: (a) no requiere llamada extra, (b) es determinista, (c) escala a recalls frecuentes sin costo de tokens. Si se necesita calidad semántica superior, `recall_hybrid()` ya incluye vector search.
- **`Option<MemoryType>` en vez de `MemoryType` con default** — `None` preserva backward compatibility con registros existentes. El default `Project` se aplica en `score_memories()`, no en la BD.
- **`f64::MAX` evitado en half-life** — usar `10_000.0` días (~27 años) da `recency ≈ 0.99993` para memorias de 1 año. Suficientemente cercano a 1.0 sin riesgo de overflow/NaN en `exp()`.
- **`ScoredMemory` struct separado** — no polucionamos `MemoryEntry` con campos transientes (`score`, `freshness_warning`). `MemoryEntry` sigue siendo el tipo de storage; `ScoredMemory` es el tipo de presentación.
- **`already_surfaced` en sesión, no en BD** — tracking de "ya mostrado en este turno/sesión" es efímero por naturaleza. Persistirlo agregaría complejidad sin beneficio (en un reinicio, repetir memorias es aceptable).
