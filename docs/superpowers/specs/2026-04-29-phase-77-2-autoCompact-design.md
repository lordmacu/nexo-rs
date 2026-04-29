## Spec: 77.2 — autoCompact (token + time triggered)

### Descripción

Loop-level autocompact que dispara LLM summarization cuando los tokens
estimados cruzan `token_pct` del context window O la sesión excede
`max_age_minutes`. Comprime los turns más antiguos no-protegidos en un
bloque de resumen, preservando el tail reciente verbatim.

### Alcance (IN)

- **Dos disparadores independientes** en el `CompactPolicy` trait:
  1. Token pressure: `estimated_tokens >= context_window * token_pct`
  2. Age: `Utc::now() - session.created_at >= max_age_minutes`
- **Sub-config `auto`** bajo `compaction` en YAML con `token_pct`,
  `max_age_minutes`, `buffer_tokens`, `min_turns_between`,
  `max_consecutive_failures`.
- **Circuit breaker** — N fallos consecutivos de compactación en el
  mismo goal → dejar de intentar. Contador persiste en
  `CompactionRuntime` (en-memoria, por goal; sobrevive mientras el
  goal está activo).
- **Age tracking** — usa `Session.created_at` (ya existe, `DateTime<Utc>`).
  El orchestrator lee `created_at` del goal/session y lo pasa en
  `CompactContext`.
- **Evento `AutoCompactFired`** — extiende `DriverEvent::CompactRequested`
  con `before_tokens`, `after_tokens`, `age_minutes`, y `trigger`
  (token vs age).
- **Integración en ambos caminos**:
  - Driver-loop orchestrator: extender la llamada a `compact_policy.classify()`
    para pasar `age_minutes` y `created_at`.
  - Core agent (`llm_behavior.rs`): agregar age-check antes del token-check
    existente en el pre-flight trigger.
- **Test de integración** — 50-turn synthetic goal que se mantiene bajo
  el token cap gracias a autoCompact.

### Fuera de alcance (OUT)

- Memory flush pre-compaction (eso es 77.5 extractMemories).
- Session-memory compact (eso es 77.3).
- Content-clear de tool results viejos por age (eso ya lo hace 77.1
  microCompact; age trigger aquí dispara summarization completa, no
  content-clear).
- GrowthBook/feature-flags — todo por YAML config + hot-reload (Phase 18).
- Cambiar el comportamiento de `find_safe_boundary` — ya protege el tail,
  y el history no contiene tool messages (se reconstruyen in-flight), así
  que no hay riesgo de cortar tool_use/tool_result pairs.
- Migración de sesiones existentes — `created_at` ya existe en `Session`.

### Interfaces

#### Config (YAML)

```yaml
llm:
  context_optimization:
    compaction:
      enabled: false
      compact_at_pct: 0.75          # legacy, sin cambios
      tail_keep_tokens: 20000       # legacy, sin cambios
      tool_result_max_pct: 0.30     # legacy, sin cambios
      summarizer_model: ""          # legacy, sin cambios
      lock_ttl_seconds: 300         # legacy, sin cambios
      micro:
        threshold_bytes: 16384
        provider: ""
        summary_max_chars: 2000
      auto:                          # NUEVO
        token_pct: 0.80              # dispara si tokens >= 80% del context window
        max_age_minutes: 120          # dispara si sesión > 120 min
        buffer_tokens: 13000          # margen bajo effective_window (como el leak)
        min_turns_between: 5          # anti-storm
        max_consecutive_failures: 3   # circuit breaker
```

Campo `auto` es `Optional<AutoCompactionConfig>`. Si no está presente,
el age trigger está deshabilitado (solo aplica token trigger legacy).
Si está presente y `token_pct > 0`, ambos triggers aplican.

#### Structs nuevos

```rust
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AutoCompactionConfig {
    #[serde(default = "default_auto_token_pct")]
    pub token_pct: f32,                    // 0.80
    #[serde(default = "default_auto_max_age_minutes")]
    pub max_age_minutes: u64,              // 120
    #[serde(default = "default_auto_buffer_tokens")]
    pub buffer_tokens: u64,                // 13000
    #[serde(default = "default_auto_min_turns_between")]
    pub min_turns_between: u32,            // 5
    #[serde(default = "default_auto_max_consecutive_failures")]
    pub max_consecutive_failures: u32,     // 3
}
```

#### `CompactContext` extendido

```rust
#[derive(Clone, Debug)]
pub struct CompactContext<'a> {
    pub goal_id: GoalId,
    pub turn_index: u32,
    pub usage: &'a BudgetUsage,
    pub context_window: u64,
    pub last_compact_turn: Option<u32>,
    pub goal_description: &'a str,
    // NUEVOS:
    pub session_age_minutes: u64,              // derivado de Session.created_at
    pub auto_config: Option<&'a AutoCompactionConfig>,  // None = auto disabled
}
```

#### `CompactPolicy::classify` retorno extendido

```rust
#[derive(Debug, Clone)]
pub enum CompactTrigger {
    TokenPressure { pct: f64, tokens_used: u64, context_window: u64 },
    Age { age_minutes: u64, max_age_minutes: u64 },
}

// classify ahora devuelve Option<(String, CompactTrigger)>
// en vez de Option<String>
```

#### Evento extendido

```rust
// DriverEvent::CompactRequested gana campos nuevos:
CompactRequested {
    goal_id: GoalId,
    turn_index: u32,
    focus: String,
    token_pressure: f64,
    // NUEVOS:
    before_tokens: u64,
    age_minutes: u64,
    trigger: CompactTrigger,
}
```

El campo `after_tokens` se emite en un evento separado post-compact
(`CompactCompleted`) o se agrega al mismo evento cuando la compactación
termina. La spec 77.2 pide `{ goal_id, before_tokens, after_tokens, age_minutes }`
— `after_tokens` solo se conoce después del LLM call del summarizer.

**Decisión:** agregar `DriverEvent::CompactCompleted` con `after_tokens`.
El evento `CompactRequested` lleva `before_tokens` + `age_minutes` + `trigger`.
`CompactCompleted` lleva `after_tokens`. El operador puede correlacionar
por `goal_id` + `turn_index`.

#### `CompactionRuntime` extendido

```rust
pub struct CompactionRuntime {
    // existentes:
    pub enabled: bool,
    pub compact_at_tokens: u64,
    pub tail_keep_chars: usize,
    pub tool_result_max_chars: usize,
    pub micro_threshold_bytes: usize,
    pub micro_summary_max_chars: usize,
    pub micro_model: String,
    pub lock_ttl_seconds: u32,
    pub summarizer_model: String,
    // NUEVOS:
    pub auto_token_pct: f32,
    pub auto_max_age_minutes: u64,
    pub auto_buffer_tokens: u64,
    pub auto_min_turns_between: u32,
    pub auto_max_consecutive_failures: u32,
    // Runtime state:
    pub consecutive_failures: u32,  // circuit breaker, resetea en éxito
}
```

### Casos de uso

#### 1. Happy path — token pressure trigger

1. Goal lleva 20 turns, cada turn agrega ~8K tokens.
2. `tokenCountWithEstimation` reporta 170K tokens en modelo de 200K context window.
3. `token_pct = 0.80` → threshold = 160K.
4. `170K >= 160K` → `CompactPolicy::classify` devuelve `Some(CompactTrigger::TokenPressure { ... })`.
5. Orchestrator publica `CompactRequested { before_tokens: 170000, trigger: TokenPressure, ... }`.
6. Se inyecta compact turn con `/compact continue goal: <desc>`.
7. Summarizer comprime los primeros 15 turns → summary de ~1K tokens.
8. Post-compact: `after_tokens ≈ 45K`. Se publica `CompactCompleted { after_tokens: 45000 }`.
9. Goal continúa normalmente.

#### 2. Happy path — age trigger

1. Goal de WhatsApp, 5 turns ligeros (15K tokens), pero lleva 3 horas abierto.
2. `max_age_minutes = 120`, `Utc::now() - session.created_at = 185 min`.
3. `CompactPolicy::classify` devuelve `Some(CompactTrigger::Age { age_minutes: 185, max_age_minutes: 120 })`.
4. Mismo flujo que token pressure: evento + compact turn + summarizer + `CompactCompleted`.

#### 3. Error path — circuit breaker

1. Goal con contexto corrupto (tool results gigantes no compactables).
2. Intento 1 de autoCompact → summarizer falla (API 500).
3. Intento 2 → summarizer falla (timeout).
4. Intento 3 → summarizer falla (prompt too long).
5. `consecutive_failures = 3 >= max_consecutive_failures` → circuit breaker tripped.
6. Siguientes turns: `classify` retorna `None` (autoCompact skipped).
7. `tracing::warn!("autoCompact circuit breaker tripped after 3 failures for goal {goal_id}")`.
8. Goal sigue corriendo sin compactación (eventualmente se irá por budget exhaust o context overflow).

#### 4. Edge case — ambos triggers simultáneos

1. Goal viejo (200 min) Y con muchos tokens (85% context).
2. `classify` retorna el primer trigger que encuentra (token pressure primero, por ser más urgente).
3. El trigger age se evaluará en el próximo turno si el token pressure no disparó efectivamente.

#### 5. Edge case — min_turns_between

1. autoCompact disparó en turn 15.
2. Turn 16: presión de tokens sigue alta pero `turn_index - last_compact_turn = 1 < 5`.
3. `classify` retorna `None`.
4. Turn 20: gap = 5 ≥ 5, `classify` vuelve a disparar.

### Dependencias

- Crates nuevos: ninguno.
- Crates del workspace modificados:
  - `crates/config` — `AutoCompactionConfig` en `types/llm.rs`
  - `crates/core` — `CompactPolicy` trait movido de driver-loop, `CompactTrigger` enum, `CompactionRuntime` extendido, age check en `llm_behavior.rs`
  - `crates/driver-loop` — delegar a trait en core, `CompactContext` extendido, evento `CompactCompleted`
  - `crates/memory` — sin cambios (el store ya soporta locks y audit)
  - `docs/` — actualizar `compact-tiers.md`

### Decisiones de diseño

1. **`CompactPolicy` trait se mueve a `crates/core/`** — actualmente en driver-loop, pero el core agent (`llm_behavior.rs`) también necesita disparar autocompact. Un solo trait compartido evita divergencia. El driver-loop inyecta la política via `Arc<dyn CompactPolicy>`.

2. **Age se mide desde `Session.created_at`, no desde el último mensaje** — el leak mide gap desde último assistant message (en microCompact.ts). La spec 77.2 explícitamente pide "edad del goal", no "inactividad". `Session.created_at` ya existe y es más simple. Si el goal se reanuda tras restart, `created_at` es el timestamp original (persiste en SQLite).

3. **Sub-config `auto` es `Option`** — si no está presente, el age trigger está deshabilitado. Esto mantiene backward compat: los YAML existentes sin `auto` solo usan el token trigger legacy (`compact_at_pct`). El `CompactPolicy` default usa `auto.token_pct` cuando `auto` está presente, sino `compact_at_pct` legacy.

4. **`buffer_tokens` aplica sobre effective context window** — igual que el leak: `effective_window = context_window - max_output_tokens`. El threshold real es `effective_window - buffer_tokens`. Esto reserva espacio para el output del summarizer (~20K tokens).

5. **Circuit breaker en-memoria, no en SQLite** — los fallos consecutivos son runtime state. Si el daemon se reinicia, el contador se pierde y el breaker resetea. Esto es aceptable: el reinicio ya resolvió potenciales corruptos de estado. Si el problema persiste, el breaker se re-activará en 3 turnos.

6. **Dos eventos separados (`CompactRequested` + `CompactCompleted`)** — `before_tokens` se conoce al disparar, `after_tokens` solo después de la compactación. Eventos separados permiten al operador ver el "delta" sin polling. Alternativa descartada: un solo evento emitido post-compact con ambos campos — pierde la señal de "se va a compactar".

7. **No se toca `find_safe_boundary`** — ya protege el tail. El history de `Session` solo tiene `User`/`Assistant` text turns; los tool messages se reconstruyen in-flight. No hay riesgo de cortar tool_use/tool_result pairs. Verificado en `compaction.rs:10-17`.
