# Memdir scanner

`memdir` scanner support is currently documented through the MCP server
extension flow and OpenClaw-parity references.

Current status:
- scanner-style memory path logic is referenced in
  `docs/src/extensions/mcp-server.md` (`teamMemPaths` parity notes)
- there is no standalone operator CLI page yet for a dedicated
  `memdir scan` command

## What operators should do today

1. Use the MCP server extension docs as the canonical path for memory
   directory layout and exposure behavior.
2. Rely on existing memory docs for storage/runtime semantics:
   - [Long-term memory (SQLite)](../memory/long-term.md)
   - [Vector search](../memory/vector.md)
3. Track roadmap follow-ups in `PHASES.md` / `FOLLOWUPS.md` for an
   explicit scanner command surface.

## Configuration — `memory.secret_guard` (C5)

The Phase 77.7 secret scanner blocks memory writes that contain API
keys, tokens, or private keys. From C5 onwards, operators control
its behaviour via the `memory.secret_guard` block in
`config/memory.yaml`:

```yaml
memory:
  short_term: { ... }
  long_term:  { ... }

  # C5 — secret-scanner policy (provider-agnostic).
  # Omit the entire block for the secure default (enabled=true,
  # on_secret=block, rules=all, exclude_rules=[]).
  secret_guard:
    enabled: true              # master switch (default true)
    on_secret: block           # block | redact | warn (default block)
    rules: all                 # "all" or a list of rule IDs
    exclude_rules: []          # list of rule IDs to skip (default empty)
```

| Field | Type | Default | Effect |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. `false` makes every check a no-op. |
| `on_secret` | `block` \| `redact` \| `warn` | `block` | What to do on detection. `block` returns an error and the write is refused; `redact` replaces matched secrets with `[REDACTED:rule_id]` and writes; `warn` writes intact and emits a warn log + event. |
| `rules` | `"all"` or `[rule_id, ...]` | `"all"` | Which rules to apply. List form selects only the named rules. |
| `exclude_rules` | `[rule_id, ...]` | `[]` | Rule IDs to silence (false positives). |

YAML-typo values (`on_secret: deny`, malformed `rules`, etc.) fail
boot loud — never silent.

### Provider-agnostic

The scanner detects API keys for every supported LLM provider
(Anthropic, MiniMax, OpenAI, Gemini, DeepSeek, xAI, Mistral) using
the same regex set. `exclude_rules` operates on rule IDs (kebab-case
like `github-pat`, `aws-access-token`, `openai-api-key`), not on
providers — silencing one rule narrows by *pattern shape*, not by
LLM-provider identity.

### Common operator workflows

- **Switch from block to warn** (e.g. dev environment debugging):
  ```yaml
  secret_guard: { on_secret: warn }
  ```
  Memory writes are not refused; the daemon logs every detection
  for review.
- **Suppress a known false positive**:
  ```yaml
  secret_guard:
    exclude_rules: [github-pat]
  ```
  All other 35 rules stay active.
- **Hard-disable for an isolated test** (NOT recommended in
  production):
  ```yaml
  secret_guard: { enabled: false }
  ```

### Prior art (validated, not copied)

- `upstream agent CLI,
  596-615,312-324` — hardcoded scanner with no YAML knob;
  activation via build flag (`feature('TEAMMEM')`) only. Operator
  override impossible without recompile. We adopt a richer
  operator-facing config rather than the hardcoded model.
- `research/src/config/zod-schema.ts` — OpenClaw uses 2-value
  enums (`redactSensitive: off|tools`, `mode: enforce|warn`). We
  extend to 3 (`block|redact|warn`) for richer behaviour without
  forcing operators to choose between block and disabled.
