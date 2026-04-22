# Ejecutar

**Feature:** $ARGUMENTS

## Tu rol

Implementa el plan aprobado. Sigue el orden exacto del plan. Un paso a la vez. Marca cada paso completado antes de continuar.

## Proceso

1. Lee el plan aprobado del tema
2. Lee `proyecto/design-agent-framework.md` — respeta convenciones de la arquitectura
3. Para cada paso del plan:
   - Implementa
   - Corre `cargo build` — sin errores antes de continuar
   - Corre tests relevantes
   - Marca paso como completado
4. Al final: `cargo test --workspace`

## Reglas de implementación

- No hardcodear API keys — siempre `${ENV_VAR}` en config
- No usar `natsio` — usar `async-nats`
- Circuit breaker en toda llamada externa
- No agregar features fuera del plan — si aparece algo nuevo, anótalo para el próximo brainstorm
- Commits atómicos por paso completado

## Referencia OpenClaw

Si el plan menciona referencia en OpenClaw (`research/`), lee el archivo exacto antes de implementar — no reimplementes algo que ya está resuelto.

## Output al terminar

```
## Implementación: <feature>

### Completado
- [x] paso 1 — `archivo` — qué se hizo
- [x] paso 2 — ...

### Tests
- `cargo test --workspace` — resultado

### Pendiente / follow-up
- <algo que surgió para próximo brainstorm>
```
