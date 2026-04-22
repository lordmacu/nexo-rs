# Brainstorm

**Topic:** $ARGUMENTS

## Tu rol

Genera ideas para el tema dado. Analiza OpenClaw como referencia: qué hicieron, cómo lo hicieron, qué cortaron, qué puedes tomar o mejorar.

## Proceso

1. Lee `proyecto/design-agent-framework.md` para entender el sistema actual
2. Explora `research/` (OpenClaw) buscando lo relevante al tema:
   - `research/VISION.md` — dirección del producto
   - `research/src/` — implementaciones core (channels, agents, plugins, memory, broker)
   - `research/extensions/` — plugins bundled
   - `research/docs/` — decisiones de diseño documentadas
   - `research/AGENTS.md` — arquitectura y reglas
3. Extrae: qué funciona bien en OpenClaw, qué cortaron deliberadamente, qué limitaciones tiene (TypeScript, single-process, etc.)
4. Genera ideas para el framework Rust: qué adoptar, qué mejorar, qué descartar

## Output

```
## Ideas para: <tema>

### Lo que hizo OpenClaw
- <hallazgo> → <archivo:línea>
- ...

### Lo que cortaron / limitaciones
- ...

### Ideas para el framework Rust
1. <idea> — ventaja sobre OpenClaw
2. ...

### Ideas descartadas
- <idea> — por qué no
```

## Siguiente paso

Cuando termines el brainstorm, corre `/spec <tema>` para formalizar las mejores ideas.
