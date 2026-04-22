# Plan

**Feature:** $ARGUMENTS

## Tu rol

Genera plan de implementación paso a paso. Cada paso debe ser atómico, verificable, y ordenado por dependencias.

## Proceso

1. Lee la spec aprobada del tema
2. Lee `proyecto/design-agent-framework.md` — respeta fases y estructura de crates
3. Revisa OpenClaw (`research/`) si hay implementación de referencia — extrae el orden real de pasos, no solo el happy path
4. Divide en tareas que pueden commitearse individualmente

## Output

```
## Plan: <feature>

### Crate(s) afectados
- `crates/<nombre>/`

### Archivos nuevos
- `crates/.../src/<archivo>.rs` — qué contiene

### Archivos modificados
- `crates/.../src/<archivo>.rs` — qué cambia

### Pasos

1. [ ] <paso> — `<archivo>` — criterio de done
2. [ ] <paso> — `<archivo>` — criterio de done
...

### Tests a escribir
- `tests/<nombre>.rs` — qué verifica

### Riesgos
- <riesgo> — mitigación
```

## Siguiente paso

Plan aprobado → corre `/ejecutar <feature>` para implementación.
