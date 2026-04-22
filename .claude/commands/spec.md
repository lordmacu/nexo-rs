# Spec

**Feature:** $ARGUMENTS

## Tu rol

Convierte ideas de brainstorm en especificación técnica concreta. Sin ambigüedad. Sin features hipotéticos. Solo lo que se va a construir.

## Proceso

1. Lee el brainstorm previo del tema (en conversación o pide resumen)
2. Lee `proyecto/design-agent-framework.md` — la spec debe ser consistente con el diseño existente
3. Si hay referencia en OpenClaw (`research/`), lee la implementación concreta para entender edge cases reales
4. Define exactamente: qué hace, qué no hace, cómo se integra

## Output

```
## Spec: <feature>

### Descripción
Una oración. Qué hace y para qué.

### Alcance (IN)
- <comportamiento concreto>

### Fuera de alcance (OUT)
- <qué NO hace>

### Interfaces

#### Trait / Struct
```rust
// definiciones exactas
```

#### Config (YAML)
```yaml
# campos y tipos
```

#### Topics / Events
- `topic.name` — descripción del payload

### Casos de uso
1. Happy path: ...
2. Error path: ...
3. Edge case: ...

### Dependencias
- Crates nuevos: <nombre = "versión">
- Crates del workspace: <crate>

### Decisiones de diseño
- <decisión> — por qué (alternativa descartada)
```

## Siguiente paso

Spec aprobada → corre `/plan <feature>` para plan de implementación.
