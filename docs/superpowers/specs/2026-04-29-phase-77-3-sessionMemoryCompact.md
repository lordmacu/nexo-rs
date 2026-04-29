## Spec: 77.3 — sessionMemoryCompact + postCompactCleanup

### Descripcion

Persiste el summary de cada compact en long-term memory para que sesiones
reanudadas inyecten el contexto comprimido sin re-ejecutar los turns
elididos. Limpia caches post-compact para evitar state leak.

### Alcance (IN)

- **CompactSummaryStore trait** — abstraccion sobre long-term memory para
  guardar/cargar summaries por `(goal_id, agent_id)`.
- **SqliteCompactSummaryStore** — impl concreto usando `nexo_memory::long_term::LongTermMemory::remember()` + `recall()`.
- **Persist on CompactCompleted** — el orchestrator guarda el summary
  (extraido del `final_text` del compact turn) en el store.
- **Inject on goal resume** — antes de `run_goal`, si hay summary
  persistido, inyectarlo como `AttemptParams.extras["compact_summary"]`
  para que `attempt.rs` lo meta en el prompt.
- **Compact boundary markers en transcript** — `TranscriptLine::CompactBoundary` con `uuid`, `token_count`, `turn_index`.
- **`PostCompactCleanup`** — `clear()` en: `MicroCompactState` (77.1),
  speculative tool cache, classifier approvals cache.
- **Evento `CompactSummaryStored`** — NATS subject
  `agent.driver.compact.summary_stored`.
- **Config thresholds** en `CompactPolicyConfig.auto.sm_compact`:
  `min_tokens` (default 10_000), `max_tokens` (default 40_000).

### Fuera de alcance (OUT)

- Usar extractMemories (77.5) como fuente del summary — se usa el output
  del compact LLM turn.
- `adjustIndexToPreserveAPIInvariants` — el tool_use/tool_result pairing
  ya lo maneja `llm_behavior.rs`.
- GrowthBook remote config — YAML config es suficiente.
- Re-compactar summaries existentes (merge de summaries viejos).

### Interfaces

#### Trait
```rust
#[async_trait]
pub trait CompactSummaryStore: Send + Sync + 'static {
    /// Persist a compact summary. `turn_index` is the turn when the
    /// compact occurred.
    async fn store(
        &self,
        goal_id: GoalId,
        agent_id: &str,
        summary: &str,
        turn_index: u32,
        before_tokens: u64,
        after_tokens: u64,
    ) -> Result<(), DriverError>;

    /// Load the most recent compact summary for a goal.
    async fn load(
        &self,
        goal_id: GoalId,
    ) -> Result<Option<CompactSummary>, DriverError>;
}

pub struct CompactSummary {
    pub agent_id: String,
    pub summary: String,
    pub turn_index: u32,
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub stored_at: chrono::DateTime<chrono::Utc>,
}
```

#### Config (YAML)
```yaml
compact_policy:
  auto:
    sm_compact:                       # Phase 77.3 (optional)
      min_tokens: 10000               # min tokens to keep post-compact
      max_tokens: 40000               # hard cap post-compact
      store_in_long_term_memory: true # persist summary to Phase 5.3 store
```

#### Topics / Events
- `agent.driver.compact.summary_stored` — payload:
  `{ goal_id, turn_index, before_tokens, after_tokens }`

### Casos de uso

1. **Happy path**: autoCompact dispara (77.2) → compact turn exitoso →
   orchestrator extrae summary de `final_text` → persiste via
   `CompactSummaryStore::store()` → emite `CompactSummaryStored` →
   `postCompactCleanup` limpia caches.

2. **Session resume**: goal reattach (Phase 71) → `run_goal` llama
   `CompactSummaryStore::load(goal_id)` → si hay summary, lo inyecta en
   `AttemptParams.extras["compact_summary"]` → `attempt.rs` lo inserta
   como system message antes del primer turn.

3. **Error path**: compact turn falla → breaker incrementa (77.2) → no
   se persiste summary → no se emite `CompactSummaryStored`.

4. **Edge case**: goal completado → summaries viejos se limpian con
   `forget()` al hacer `cleanup_on_done`.

### Dependencias
- Crates del workspace: `nexo-driver-loop`, `nexo-driver-types`,
  `nexo-memory`, `nexo-core`
- Sin nuevas dependencias externas.

### Decisiones de diseno

- **Summary del LLM, no de extractMemories** — 77.5 no existe aun.
  Cuando llegue, se puede enriquecer el summary con session memory.
  Alternativa descartada: esperar a 77.5 (bloquearia 77.3).
- **Store en driver-loop, no en core** — el orchestrator es el unico
  que sabe cuando un compact ocurrio. Alternativa descartada: store en
  `llm_behavior.rs` (el core agent no tiene acceso al summary completo).
- **`CompactSummaryStore` como trait** — permite noop/mock en tests y
  cambiar backend sin tocar el orchestrator. Alternativa descartada:
  llamar directo a `LongTermMemory` (acoplamiento fuerte).
- **Transcript boundary markers** — `TranscriptLine::CompactBoundary`
  en vez de mutar entries existentes. Mantiene el audit trail inmutable.
