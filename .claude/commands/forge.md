# Forge — Orquestador de desarrollo

**Uso:** `/forge <fase-nombre>`  
Ejemplo: `/forge brainstorm circuit-breaker` o `/forge spec heartbeat` o `/forge plan memory` o `/forge ejecutar llm-minimax`

## Qué hace

Orquesta el flujo completo de desarrollo:

```
brainstorm → spec → plan → ejecutar
```

Detecta la fase en $ARGUMENTS y ejecuta la skill correspondiente con el contexto correcto.

## Flujo

### Si la fase es `brainstorm <tema>`

1. Lee `proyecto/design-agent-framework.md`
2. Explora `research/` (OpenClaw) buscando lo relevante al tema:
   - `research/VISION.md`, `research/AGENTS.md`
   - `research/src/` — core (channels, agents, plugins, memory)
   - `research/extensions/` — plugins bundled
   - `research/docs/` — decisiones documentadas
3. Extrae: qué hizo OpenClaw, qué cortaron, qué limitaciones tiene
4. Genera ideas para el framework Rust
5. Output formato brainstorm
6. Al final: **"Listo para `/forge spec <tema>`"**

### Si la fase es `spec <tema>`

1. Lee brainstorm previo en conversación
2. Lee `proyecto/design-agent-framework.md`
3. Lee implementación OpenClaw si existe (`research/`)
4. Genera spec técnica completa (interfaces Rust, config YAML, topics, casos de uso, decisiones)
5. Al final: **"Listo para `/forge plan <tema>`"**

### Si la fase es `plan <tema>`

1. Lee spec aprobada
2. Lee `proyecto/design-agent-framework.md`
3. Revisa OpenClaw para orden real de pasos
4. Genera plan atómico (archivos nuevos, modificados, pasos con criterio de done, tests, riesgos)
5. Al final: **"Listo para `/forge ejecutar <tema>`"**

### Si la fase es `ejecutar <tema>`

1. Lee plan aprobado
2. Lee `proyecto/design-agent-framework.md`
3. Implementa paso a paso: implementa → `cargo build` → test → marca done → siguiente
4. No agrega features fuera del plan
5. Commits atómicos por paso
6. Al final: reporte de completado + follow-ups para próximo brainstorm

## Progress tracking (obligatorio)

Después de completar cada sub-fase durante `/forge ejecutar`:
1. Abre `proyecto/PHASES.md` → marca la sub-fase con ✅
2. Abre `CLAUDE.md` → actualiza el contador de la fase (`0/6` → `1/6`) y el total global
3. Si la fase entera está completa, marca la fila completa en la tabla

Formato en PHASES.md:
```
### 1.1 — Workspace scaffold   ✅
### 1.2 — Config loading       🔄
### 1.3 — Local event bus      ⬜
```

No continúes al siguiente paso sin marcar el anterior como done.

## Reglas globales (todas las fases)

- OpenClaw (`research/`) = referencia, no destino. Rust > TypeScript, microservices > single-process
- No hardcodear API keys
- No usar `natsio` — usar `async-nats`
- Circuit breaker en toda llamada externa
- MiniMax es el LLM primario
- Si algo nuevo aparece durante ejecución → anótalo, no lo implementes
