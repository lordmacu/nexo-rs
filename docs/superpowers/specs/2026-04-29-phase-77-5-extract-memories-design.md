## Spec: 77.5-extractMemories

### Descripción
Post-turn LLM extraction that reads recent turn transcript and writes
durable memories to the auto-memory directory (`MEMORY.md` +
`memory/*.md`), complementing Phase 10.6 dreaming with an inline path.

### Alcance (IN)

- **Post-turn trigger**: after every successful turn (Done / NeedsRetry),
  fire extractMemories as a fire-and-forget mini-LLM call
- **Limited tool set**: Read, Grep, Glob, read-only Bash, Write/Edit
  restricted to memory directory only
- **Memory manifest pre-injection**: scan `memory/*.md` frontmatter before
  LLM call so the agent doesn't `ls` on turn 1
- **Prompt**: 4-type taxonomy (user/feedback/project/reference) +
  WHAT_NOT_TO_SAVE + frontmatter format + 2-step save instructions
- **Mutual exclusion**: skip extraction when main agent already wrote to
  memory dir this turn (detect via `tool_use` blocks targeting memory paths)
- **Coalescing**: if extraction in-progress when next turn ends, stash
  context and run ONE trailing extraction after current finishes
- **Turn throttle**: run every N eligible turns (default: 1 = every turn)
- **Hard cap**: max 5 LLM turns per extraction
- **Best-effort**: errors logged, never surfaced to user, never block
  the main loop
- **Config gating**: `extract_memories.enabled: bool` (default false) in
  `CompactPolicyConfig` (reuses existing config surface near compact tiers)

### Fuera de alcance (OUT)

- Team memory directory (Claude Code's `TEAMMEM` feature flag) — private only
- `autoDream` offline consolidation (Phase 10.6 dreaming covers that)
- PromptSuggestions / PromptCoaching (separate Claude Code features)
- Skip-index mode (`tengu_moth_copse` GrowthBook flag) — always write
  MEMORY.md index
- `drainPendingExtraction` on shutdown (first iteration — fire-and-forget
  with tokio::spawn is sufficient; data loss on crash is acceptable for
  best-effort extraction)

### Interfaces

#### Trait / Struct

```rust
/// Post-turn memory extractor. Runs a mini-LLM call with limited tools
/// to extract durable memories from the turn transcript.
#[async_trait]
pub trait ExtractMemories: Send + Sync + 'static {
    /// Trigger extraction after a turn completes.
    /// `messages` = recent turn messages visible to the model.
    /// `memory_dir` = path to the auto-memory directory.
    /// `llm` = LLM client to use (same provider as main agent).
    /// Fire-and-forget — errors are logged, never returned.
    async fn extract(
        &self,
        messages: &[TranscriptMessage],
        memory_dir: &Path,
        llm: &dyn ChatProvider,
    );
}

pub struct DefaultExtractMemories {
    pub enabled: bool,
    /// Run extraction every N eligible turns (default 1).
    pub turns_throttle: u32,
    /// Hard cap on LLM turns per extraction (default 5).
    pub max_turns: u32,
    /// Internal mutable state behind Mutex.
    state: Mutex<ExtractMemoriesState>,
}

struct ExtractMemoriesState {
    /// UUID of last processed message (cursor).
    last_message_uuid: Option<String>,
    /// True while an extraction is in progress.
    in_progress: bool,
    /// Turns since last extraction ran.
    turns_since_last: u32,
    /// Stashed context for trailing run.
    pending: Option<PendingExtraction>,
    /// Consecutive failures (circuit breaker, optional).
    consecutive_failures: u32,
}
```

#### Config (YAML)

```yaml
# In config/driver/claude.yaml, under compact_policy:
compact_policy:
  extract_memories:
    enabled: false           # default false — opt-in
    turns_throttle: 1        # run every N turns (1 = every turn)
    max_turns: 5             # hard cap on LLM turns per extraction
    max_consecutive_failures: 3  # circuit breaker (0 = disabled)
```

#### Events

- `agent.driver.extract_memories.completed` — `ExtractMemoriesCompleted {
  goal_id, turn_index, memories_saved: u32, duration_ms: u64 }`
- `agent.driver.extract_memories.skipped` — `ExtractMemoriesSkipped {
  goal_id, reason: SkipReason }` where `SkipReason` = `Disabled |
  Throttled | InProgress | CircuitBreakerOpen | MainAgentWrote`

### Casos de uso

1. **Happy path**: turn completa, extractMemories dispara. Escanea memory
   dir (frontmatter de `memory/*.md`), construye prompt con manifest +
   últimos N mensajes. LLM lee archivos relevantes, edita/escribe memory
   files, actualiza MEMORY.md index. Emite `ExtractMemoriesCompleted`.
   Duración típica: 2-4 turnos LLM.

2. **Error path**: LLM falla (timeout, rate limit, etc.). Error logged.
   Consecutive failures counter incrementa. Si llega a
   `max_consecutive_failures`, circuit breaker abre y saltos subsecuentes
   emiten `SkipReason::CircuitBreakerOpen`. Breaker resetea al próximo
   éxito.

3. **Edge case — main agent already wrote**: si el agente principal usó
   Write/Edit con target en `memory_dir` durante este turno, skip
   extracción con `SkipReason::MainAgentWrote`. Avanza cursor para no
   reprocesar estos mensajes.

4. **Edge case — coalescing**: extracción en progreso cuando nuevo turno
   termina → stash context (solo el más reciente). Cuando la extracción
   actual termine, ejecuta UNA trailing run con el context stasheado.

### Dependencias

- Crates del workspace:
  - `nexo-driver-loop` — orchestrator hook point + config
  - `nexo-driver-types` — event types + config struct
  - `nexo-core` — `TranscriptMessage`, `ChatProvider` trait
  - `nexo-memory` — `LongTermMemory` (para persistir vía FTS, opcional)
  - `nexo-llm` — LLM client (MiniMax / Anthropic)
- Sin crates nuevos.

### Decisiones de diseño

- **Mini-LLM call, no sub-goal** — extractMemories no necesita worktree,
  MCP config, ni registry slot. Una llamada LLM directa con `ToolRegistry`
  limitado es suficiente. Alternativa descartada: spawnear goal completo
  del driver-loop (overhead innecesario).

- **Integración vía PostCompactCleanup** — el placeholder de 77.3 ya tiene
  el hook point en el orchestrator. ExtractMemories lo reemplaza/expande
  en lugar de crear un nuevo punto de integración.

- **Escribir a filesystem, no solo LTM** — Claude Code escribe archivos
  `.md` en `~/.claude/projects/<path>/memory/`. Nexo ya tiene MEMORY.md
  infrastructure (Phase 10.6 dreaming escribe ahí). Mantener filesystem
  como storage primario mantiene compatibilidad con el sistema actual.
  `LongTermMemory::remember()` puede ser path secundario (opcional).

- **Prompt port directo** — los prompts de `claude-code-leak/src/services/
  extractMemories/prompts.ts` están validados con evals. Port directo a
  constantes Rust, adaptando solo los tool names (Read → file_read, etc.).
