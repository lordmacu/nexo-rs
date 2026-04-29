## Plan: 77.2 — autoCompact (token + time triggered)

### Crates afectados
- `crates/config/` — `AutoCompactionConfig` + integrar en `CompactionConfig`
- `crates/core/` — `CompactPolicy` trait movido aquí + `CompactTrigger` + extender `CompactionRuntime`
- `crates/driver-loop/` — wire new policy en orchestrator + eventos extendidos + config
- `crates/memory/` — sin cambios (store + locks ya suficientes)
- `docs/` — actualizar `compact-tiers.md`

### Archivos nuevos
- `crates/core/src/agent/compact_policy.rs` — `CompactPolicy` trait (movido de driver-loop), `CompactTrigger` enum, `DefaultCompactPolicy` con token + age triggers, `CompactContext` extendido
- `crates/core/src/agent/compact_policy_test.rs` — unit tests para los dos triggers + circuit breaker

### Archivos modificados
- `crates/config/src/types/llm.rs` — agregar `AutoCompactionConfig` struct + campo `auto` en `CompactionConfig`
- `crates/core/src/agent/mod.rs` — exportar `compact_policy` module
- `crates/core/src/agent/llm_behavior.rs` — extender `CompactionRuntime` con campos auto + circuit breaker state + age check en pre-flight trigger
- `crates/core/src/lib.rs` — re-export `CompactPolicy`, `CompactTrigger`, `DefaultCompactPolicy`, `CompactContext`
- `crates/driver-loop/src/compact.rs` — eliminar (movido a core), o mantener como re-export shim
- `crates/driver-loop/src/events.rs` — extender `CompactRequested` + agregar `CompactCompleted`
- `crates/driver-loop/src/orchestrator.rs` — wire `session_age_minutes` en `CompactContext`, manejar nuevo retorno de `classify`, emitir `CompactCompleted`
- `crates/driver-loop/src/config.rs` — agregar `auto` sub-config a `CompactPolicyConfig`
- `crates/driver-loop/src/bin/nexo_driver.rs` — wire `auto` config → `DefaultCompactPolicy`
- `crates/driver-loop/src/lib.rs` — actualizar exports (eliminar los movidos a core)
- `docs/src/ops/compact-tiers.md` — documentar triggers token + age

### Pasos

#### 1. [ ] `AutoCompactionConfig` en config types — `crates/config/src/types/llm.rs`
Agregar struct con serde defaults:
```rust
pub struct AutoCompactionConfig {
    token_pct: f32,           // default 0.80
    max_age_minutes: u64,     // default 120
    buffer_tokens: u64,       // default 13000
    min_turns_between: u32,   // default 5
    max_consecutive_failures: u32, // default 3
}
```
Agregar `auto: Option<AutoCompactionConfig>` a `CompactionConfig`.
Done: `cargo build -p nexo-config` compila, test de deserialización pasa con YAML fixture.

#### 2. [ ] Mover `CompactPolicy` trait a `core/` + agregar `CompactTrigger` — `crates/core/src/agent/compact_policy.rs`
- Crear archivo con `CompactContext` (extendido con `session_age_minutes: u64`, `auto_config: Option<&AutoCompactionConfig>`)
- `CompactTrigger` enum: `TokenPressure { pct, tokens_used, context_window }` | `Age { age_minutes, max_age_minutes }`
- `CompactPolicy` trait: `async fn classify(&self, ctx: &CompactContext<'_>) -> Option<(String, CompactTrigger)>`
- `DefaultCompactPolicy`: implementar ambos triggers. Token pressure igual que antes (usa `auto.token_pct` si available, sino `threshold` legacy). Age trigger: `session_age_minutes >= auto.max_age_minutes`. Ambos respetan `min_turns_between`.
- Re-export desde `core/src/agent/mod.rs` y `core/src/lib.rs`.
Done: `cargo build -p nexo-core` compila, trait tests pasan.

#### 3. [ ] Extender `CompactionRuntime` con campos auto + circuit breaker — `crates/core/src/agent/llm_behavior.rs`
Agregar campos a `CompactionRuntime`:
- `auto_token_pct: f32`, `auto_max_age_minutes: u64`, `auto_buffer_tokens: u64`
- `auto_min_turns_between: u32`, `auto_max_consecutive_failures: u32`
- `consecutive_failures: u32` (runtime state)
- `last_compact_turn: Option<u32>` (runtime state)

Extender `with_compaction()` o agregar `with_auto_compaction()` builder.
En pre-flight trigger (línea ~912): agregar age check antes del token check:
- Si `session.created_at` + `max_age_minutes` < `Utc::now()` → disparar compactación.
- Si `consecutive_failures >= max_consecutive_failures` → skip.
- En éxito → `consecutive_failures = 0`, `last_compact_turn = Some(turn_index)`.
- En fallo → `consecutive_failures += 1`.
Done: `cargo build -p nexo-core` compila.

#### 4. [ ] Extender eventos — `crates/driver-loop/src/events.rs`
- `CompactRequested`: agregar `before_tokens: u64`, `age_minutes: u64`, `trigger: CompactTrigger`.
- `CompactCompleted`: nuevo variant con `goal_id`, `turn_index`, `after_tokens: u64`.
- Actualizar `DriverEvent::topic_str()` para ambos.
Done: `cargo build -p nexo-driver-loop` compila.

#### 5. [ ] Wire driver-loop orchestrator — `crates/driver-loop/src/orchestrator.rs`
- `CompactContext` construction (línea ~584): agregar `session_age_minutes` derivado de `started.elapsed()`, y `auto_config` desde la política.
- Actualizar llamada a `classify()` para el nuevo retorno `(String, CompactTrigger)`.
- Emitir `CompactRequested` con `before_tokens`, `age_minutes`, `trigger`.
- Después del compact turn (cuando `last_was_compact` y el siguiente turno NO es compact), emitir `CompactCompleted` con `after_tokens` (del `usage.tokens` post-compact).
- Actualizar imports: usar `CompactPolicy`, `CompactContext`, `CompactTrigger`, `DefaultCompactPolicy` desde `nexo_core` en vez de `crate::compact`.
Done: `cargo build -p nexo-driver-loop` compila.

#### 6. [ ] Wire driver config — `crates/driver-loop/src/config.rs` + `crates/driver-loop/src/bin/nexo_driver.rs`
- `CompactPolicyConfig`: agregar `auto: Option<AutoCompactionConfig>` (re-exportado de `nexo_config`).
- `nexo_driver.rs`: construir `DefaultCompactPolicy` con `auto_config`.
Done: `cargo build -p nexo-driver` compila.

#### 7. [ ] Eliminar `driver-loop/src/compact.rs` — mover a core
El archivo original queda como shim que re-exporta desde `nexo_core::agent::compact_policy` para no romper imports internos. O eliminar completamente y actualizar todos los imports en driver-loop.
Done: `cargo build -p nexo-driver-loop` compila sin warnings.

#### 8. [ ] Integration test — 50-turn synthetic goal
En `crates/driver-loop/tests/`:
- Goal simulado con `max_age_minutes: 1`, `token_pct: 0.80`.
- 50 turns de tool calls ligeros (Bash echo con output ~1K tokens cada uno).
- Verificar que el goal completa sin `BudgetExhausted { axis: Tokens }`.
- Verificar que se emiten `CompactRequested` y `CompactCompleted`.
Done: `cargo test --workspace` pasa.

#### 9. [ ] Docs sync — `docs/src/ops/compact-tiers.md`
- Documentar triggers token + age.
- Mostrar YAML de ejemplo con sección `auto`.
- Explicar circuit breaker y `min_turns_between`.
- Ejecutar `mdbook build docs` sin broken links.
Done: `mdbook build docs` sale limpio.

### Tests a escribir

| # | Test | Ubicación | Verifica |
|---|------|-----------|----------|
| 1 | `auto_config_deserializes_with_defaults` | `config/src/types/llm.rs` tests | YAML → struct |
| 2 | `token_pressure_trigger_with_auto_config` | `core/src/agent/compact_policy_test.rs` | Token trigger usa `auto.token_pct` |
| 3 | `age_trigger_fires_when_expired` | `core/src/agent/compact_policy_test.rs` | Age trigger con `session_age > max_age` |
| 4 | `age_trigger_skips_when_not_expired` | `core/src/agent/compact_policy_test.rs` | Sin disparo bajo `max_age` |
| 5 | `both_triggers_token_wins` | `core/src/agent/compact_policy_test.rs` | Token pressure tiene prioridad |
| 6 | `min_turns_between_respected` | `core/src/agent/compact_policy_test.rs` | Anti-storm gap |
| 7 | `circuit_breaker_stops_after_n_failures` | `core/src/agent/compact_policy_test.rs` | 3 fallos → skip |
| 8 | `circuit_breaker_resets_on_success` | `core/src/agent/compact_policy_test.rs` | Éxito → contador a 0 |
| 9 | `auto_disabled_when_none` | `core/src/agent/compact_policy_test.rs` | `auto_config: None` → age trigger disabled |
| 10 | `fifty_turn_goal_stays_under_token_cap` | `driver-loop/tests/` | 50-turn integration |

### Riesgos

- **`started.elapsed()` no es `created_at` persistido** — el driver-loop mide edad desde que `run_goal` empezó. Si el daemon se reinicia, el goal se reatacha y `started` se resetea. Esto es aceptable: el age trigger del driver-loop es best-effort. El core agent (`llm_behavior.rs`) sí usa `Session.created_at` que persiste en SQLite. **Mitigación:** documentar que el age trigger en driver-loop mide wall-clock desde spawn, no desde creación original.
- **El `CompactContext` recibe `session_age_minutes` pero el driver-loop no tiene acceso a `Session`** — el orchestrator usa `Instant::now() - started`. El core agent usa `Session.created_at`. Son dos fuentes distintas. **Mitigación:** el trait acepta `session_age_minutes: u64`, cada caller lo calcula como puede.
- **`CompactCompleted.after_tokens` solo se conoce post-compact turn** — el compact turn es un turno real de LLM. El `usage.tokens` post-compact refleja el contexto comprimido. **Mitigación:** emitir `CompactCompleted` en el siguiente turno regular después del compact turn, no durante el compact turn mismo.
