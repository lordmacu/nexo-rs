# `SyntheticOutput` (Phase 79.3)

`SyntheticOutput` forces a goal to terminate with a JSON value that
matches a caller-provided JSONSchema. Closes the gap between "model
produces free prose" and "downstream consumer needs a struct" —
direct input for Phase 19/20 pollers, Phase 51 eval harness, and any
future contract-shaped goal.

Lift from
`claude-code-leak/src/tools/SyntheticOutputTool/SyntheticOutputTool.ts:1-163`.

## Diff vs leak

The leak builds **one tool per schema** via
`createSyntheticOutputTool(jsonSchema)` so the model's input *is* the
schema. Nexo-rs runs as a daemon — building a fresh tool per call
breaks tool-registry semantics. We ship a single tool whose input
carries BOTH the schema and the value:

```json
{
  "schema": { "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] },
  "value":  { "name": "ana" }
}
```

Pollers and eval harnesses inject the schema via prompt template;
ad-hoc callers pass it inline. The `terminal_schema` follow-up
(tracked in FOLLOWUPS.md) lets the runtime carry the schema and only
the model's `value` flows on the wire — closer to the leak's
single-input shape.

## Tool shape

| Arg | Type | Required | Notes |
|-----|------|----------|-------|
| `schema` | object | yes | JSONSchema (Draft 7 / 2019-09 / 2020-12). Must be a JSON object. |
| `value` | any | yes | The value to validate. Object / array / scalar — any shape the schema permits. |

## Response

```json
{
  "ok": true,
  "structured_output": <value>,
  "instructions": "Output validated. The goal can terminate now — do not call any other tool this turn unless the goal contract calls for it explicitly."
}
```

On failure the call returns an error whose body lists every
violation with its JSONPath:

```
SyntheticOutput: value does not match schema (2 errors): /age: 30 is not of type "string"; /tags/0: "purple" is not one of ["red","green"]
```

## Validation

Uses `jsonschema = "0.20"` — already an optional dep on
`nexo-core` (default-on via the `schema-validation` Cargo feature
that Phase 9.2 introduced). Builds without the feature compile, but
`SyntheticOutput` returns a clear "feature disabled" error rather
than silently passing through — synthesised output without
validation is worse than no synthesis.

## Plan-mode classification

Classified `ReadOnly` in `nexo_core::plan_mode::READ_ONLY_TOOLS`.
The tool only validates and echoes; it never touches workspace,
broker, or external state. Safe to call while plan mode is on.

## References

- **PRIMARY**:
  `claude-code-leak/src/tools/SyntheticOutputTool/SyntheticOutputTool.ts:1-163`.
- **SECONDARY**: OpenClaw `research/` — no equivalent. Single-process
  TS reference shapes its outputs via Zod parsing inline; no
  separate "force structured output" tool.
- Plan + spec: `proyecto/PHASES.md::79.3`.
