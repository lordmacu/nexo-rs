## Plan: 77.5-extractMemories

### Crate(s) afectados
- `crates/driver-types/` — `ExtractMemoriesConfig` struct
- `crates/driver-loop/` — core extraction module + events + orchestrator wiring + config
- `docs/` — actualizar `compact-tiers.md`

### Archivos nuevos
- `crates/driver-loop/src/extract_memories.rs` — `ExtractMemories` struct, state machine,
  prompt builder, memory manifest scanner, single-turn LLM extraction loop
- `crates/driver-loop/src/extract_memories_prompt.rs` — prompt constants (ported from
  `claude-code-leak/src/services/extractMemories/prompts.ts` + `memoryTypes.ts`)

### Archivos modificados
- `crates/driver-types/src/compact_policy.rs` — `ExtractMemoriesConfig` struct + re-export
- `crates/driver-types/src/lib.rs` — re-export `ExtractMemoriesConfig`
- `crates/driver-loop/src/events.rs` — `ExtractMemoriesCompleted` + `ExtractMemoriesSkipped` variants
- `crates/driver-loop/src/orchestrator.rs` — wire extraction call in post-turn hook
- `crates/driver-loop/src/config.rs` — `extract_memories` field in `CompactPolicyConfig`
- `crates/driver-loop/src/post_compact_cleanup.rs` — evolve from no-op to call `ExtractMemories`
- `crates/driver-loop/src/lib.rs` — register new modules
- `docs/src/ops/compact-tiers.md` — document Tier 4 extractMemories

### Pasos

#### 1. [ ] `ExtractMemoriesConfig` — `crates/driver-types/src/compact_policy.rs`
Agregar al final del archivo:
```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExtractMemoriesConfig {
    #[serde(default)]
    pub enabled: bool,                    // default false
    #[serde(default = "default_extract_throttle")]
    pub turns_throttle: u32,              // default 1
    #[serde(default = "default_extract_max_turns")]
    pub max_turns: u32,                   // default 5
    #[serde(default = "default_extract_max_failures")]
    pub max_consecutive_failures: u32,    // default 3
}
```
Re-export desde `lib.rs`.
Done: `cargo build -p nexo-driver-types` compila.

#### 2. [ ] Wire config — `crates/driver-loop/src/config.rs`
Agregar campo `extract_memories: Option<ExtractMemoriesConfig>` en `CompactPolicyConfig`
+ `#[serde(default)]` + `None` en Default impl.
Done: `cargo build -p nexo-driver-loop` compila.

#### 3. [ ] Events — `crates/driver-loop/src/events.rs`
Agregar variants:
```rust
ExtractMemoriesCompleted {
    goal_id: GoalId,
    turn_index: u32,
    memories_saved: u32,
    duration_ms: u64,
},
ExtractMemoriesSkipped {
    goal_id: GoalId,
    reason: ExtractSkipReason,
},
```
NATS subjects: `agent.driver.extract_memories.completed` / `skipped`.
Agregar `ExtractSkipReason` enum: `Disabled | Throttled | InProgress | CircuitBreakerOpen | MainAgentWrote`.
Done: compila.

#### 4. [ ] Prompt constants — `crates/driver-loop/src/extract_memories_prompt.rs`
Port directo de `claude-code-leak/src/services/extractMemories/prompts.ts` +
`claude-code-leak/src/memdir/memoryTypes.ts`:
- `TYPES_SECTION` — 4-type taxonomy (user/feedback/project/reference) con XML-like type blocks
- `WHAT_NOT_TO_SAVE_SECTION` — exclusion list
- `MEMORY_FRONTMATTER_EXAMPLE` — markdown frontmatter template
- `build_extract_prompt(new_message_count, existing_memories) -> String`
Adaptar tool names a Nexo (Read → `file_read`, Write → `file_write`, Edit → `file_edit`,
Grep → `grep`, Glob → `glob`, Bash → `bash`).
Done: `cargo build -p nexo-driver-loop` compila.

#### 5. [ ] Core extractor — `crates/driver-loop/src/extract_memories.rs`
Struct + impl:
- `ExtractMemories` struct: `config: ExtractMemoriesConfig`, `state: Mutex<ExtractMemoriesState>`
- `ExtractMemoriesState`: `last_message_uuid`, `in_progress`, `turns_since_last`,
  `pending: Option<PendingContext>`, `consecutive_failures`
- `scan_memory_manifest(memory_dir: &Path) -> String` — lee `memory/*.md` frontmatter
  (name, description, type fields), formatea como lista `[type] filename: description`
- `build_prompt(messages, manifest, memory_dir) -> String` — wrapper around
  `extract_memories_prompt::build_extract_prompt`
- `run_extraction(&self, messages, memory_dir, llm)` — single-turn LLM call:
  1. Build prompt with manifest
  2. Call LLM (structured JSON output: `[{file_path, content}]`)
  3. Write files to memory_dir
  4. Update MEMORY.md index (add pointers for new files)
  5. Update state (cursor, consecutive_failures, etc.)
- `extract(&self, ...)` — public entry point: gate checks (enabled, throttle, in_progress,
  circuit breaker, main_agent_wrote), then spawn `tokio::spawn(run_extraction(...))`
- `has_memory_writes_since(messages, last_uuid, memory_dir) -> bool` — detect if main
  agent already wrote to memory dir
- `skip_reason_to_event(reason) -> DriverEvent` — map skip reason to event

Single-turn approach: LLM receives structured output instruction. Response parsed as
JSON array of `{file_path, content}` objects. Write each to disk. Update MEMORY.md
index for new files. This collapses the 2-turn Read→Write pattern into 1 turn
because the manifest pre-injection eliminates the need for `ls`/Read exploration.

Done: `cargo build -p nexo-driver-loop` compila.

#### 6. [ ] Wire into orchestrator — `crates/driver-loop/src/orchestrator.rs`
- Add `extract_memories: Option<Arc<ExtractMemories>>` field to orchestrator
- Builder: `.extract_memories(e)` setter
- In `build()`: construct `ExtractMemories` from `compact_policy.extract_memories` config
  if `Some` and `enabled`
- In `run_goal`, after successful turn (Done / NeedsRetry outcomes in the match block):
  call `self.extract_memories.extract(messages, memory_dir, llm)` fire-and-forget
  (via `tokio::spawn` if sync, or direct `.await` since it spawns internally)
- Pass relevant context: recent messages, memory dir path, LLM client reference

Update `post_compact_cleanup.rs`: `PostCompactCleanup.run()` now takes optional
`ExtractMemories` reference and delegates to it.

Done: `cargo build -p nexo-driver-loop` compila.

#### 7. [ ] Register modules — `crates/driver-loop/src/lib.rs`
Agregar `pub mod extract_memories;` y `pub mod extract_memories_prompt;`.
Done: compila.

#### 8. [ ] Tests
- `extract_memories_prompt.rs`: test `build_extract_prompt` output contiene secciones
  esperadas (types, what_not_to_save, frontmatter example)
- `extract_memories.rs`:
  - `scan_manifest_empty_dir_returns_empty` — dir sin archivos → string vacío
  - `scan_manifest_reads_frontmatter` — archivo con frontmatter → `[user] file.md: desc`
  - `has_memory_writes_detects_write` — mensaje con Write tool → true
  - `has_memory_writes_no_write_returns_false` — mensajes sin Write → false
  - `skip_reason_disabled` — config enabled=false → `SkipReason::Disabled`
  - `throttle_skip` — `turns_since_last < throttle` → `SkipReason::Throttled`
  - `circuit_breaker_trips_after_n_failures` — N fallos → `CircuitBreakerOpen`
- `config.rs`: test `extract_memories` block deserializa con defaults
Done: `cargo test -p nexo-driver-loop` pasa.

#### 9. [ ] Docs sync — `docs/src/ops/compact-tiers.md`
Agregar Tier 4: extractMemories — post-turn LLM memory extraction.
YAML example, events, operational intent.
Done: `mdbook build docs` limpio.

### Tests a escribir

| # | Test | Ubicación | Verifica |
|---|------|-----------|----------|
| 1 | `prompt_contains_required_sections` | `extract_memories_prompt.rs` | Prompt incluye types, what_not_to_save, frontmatter |
| 2 | `scan_manifest_empty_dir` | `extract_memories.rs` | Dir vacío → string vacío |
| 3 | `scan_manifest_reads_frontmatter` | `extract_memories.rs` | Lee name/description/type del frontmatter |
| 4 | `has_memory_writes_detects` | `extract_memories.rs` | Detecta Write tool en memory dir |
| 5 | `has_memory_writes_none` | `extract_memories.rs` | Sin Write → false |
| 6 | `skip_disabled` | `extract_memories.rs` | enabled=false → skip |
| 7 | `throttle_skip` | `extract_memories.rs` | Respeta turns_throttle |
| 8 | `breaker_trips` | `extract_memories.rs` | N fallos → breaker abre |
| 9 | `config_deserializes` | `config.rs` | Bloque YAML → struct |

### Riesgos

- **Single-turn vs multi-turn** — la spec dice "max 5 LLM turns" pero el plan usa
  single-turn con structured output. **Mitigación:** el manifest pre-injection elimina
  la necesidad de exploración; structured JSON output es determinista. Si la calidad
  de extracción resulta pobre, iterar a multi-turn con tool loop en 77.5.b.
- **LLM client availability** — el orchestrator necesita acceso al `ChatProvider`.
  **Mitigación:** pasar `Arc<dyn ChatProvider>` desde `nexo_driver.rs` (ya tiene
  acceso al provider config). Si no está disponible, feature gate.
- **Prompt token cost** — el prompt de extracción es ~3000 tokens fijos + manifest +
  mensajes del turno. **Mitigación:** throttle configuración default=1 pero
  recomendado=3+ en producción.
