# Calculator demo

Sample workspace for the Phase 67 driver subsystem. Use it to see
how the agent reads `PHASES.md` / `FOLLOWUPS.md` and how the
dispatch tools behave against a project that has nothing
implemented yet.

## What's in here

- `PHASES.md` — 5 phases × ~12 sub-phases for a terminal calculator
  (`1.1` and `1.2` shipped, the rest pending).
- `FOLLOWUPS.md` — open + resolved items mixed so the parser sees
  both shapes (strikethrough, `✅ shipped` markers).

There's no Cargo project here yet — just the markdown files. The
agent is meant to read them, not run them.

## Quick smoke from the CLI

```bash
NEXO_PROJECT_ROOT=$(pwd)/demos/calculator \
  nexo-driver-tools status
# → 🔄 1.3 — Workspace lints + edition pin   (first non-shipped)

NEXO_PROJECT_ROOT=$(pwd)/demos/calculator \
  nexo-driver-tools status --phase 2.2
# → 2.2 — Recursive descent parser
#   Pratt-style parser that respects + - < * / precedence …

NEXO_PROJECT_ROOT=$(pwd)/demos/calculator \
  nexo-driver-tools status --followups
# → H-1 [Hardening] — Fuzz the parser
#   H-2 [Hardening] — Reject NaN / inf at the AST evaluator
#   V-2 [Phase 4 — Variables] — Multi-character operators
#   …
```

## Running the agent against this folder

Set both env vars before booting `nexo`:

```bash
export NEXO_PROJECT_ROOT=$(pwd)/demos/calculator
export NEXO_DRIVER_INTEGRATED=1     # opt-in to the in-process orchestrator
export NEXO_DRIVER_CONFIG=config/driver/claude.yaml
nexo --config config/agents.yaml
```

From a paired Telegram / WhatsApp chat (or any binding with
`dispatch_capability: full`):

- "¿en qué fase va?" → agent calls `project_status` →
  `🔄 1.3 — Workspace lints + edition pin`.
- "qué followups quedan abiertos" → `followups_open` →
  H-1 / H-2 / V-2 / V-3 / P-1 / P-2.
- "programa la fase 2.1" → `program_phase 2.1` → goal admitted,
  Claude subprocess starts in a worktree of this folder. When it
  finishes, `notify_origin` lands a summary back in the chat.

## Status legend

| Glyph | Meaning |
|-------|---------|
| ✅    | Done    |
| 🔄    | In progress (run by the agent right now) |
| ⬜    | Pending |

The agent only treats one sub-phase as `🔄` at a time — when you
dispatch `program_phase` for a pending sub-phase the tracker
updates after the agent commits its work.
