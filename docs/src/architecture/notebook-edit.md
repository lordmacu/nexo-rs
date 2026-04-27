# `NotebookEdit` (Phase 79.13)

Cell-level edits on Jupyter `.ipynb` notebooks. Pure-Rust round-trip
through `serde_json::Value` — no `jupyter` binary, no Python
dependency. Unknown top-level fields survive untouched (forward-compat
with newer nbformat).

Lift from
`claude-code-leak/src/tools/NotebookEditTool/NotebookEditTool.ts:30-489`.

## Tool shape

```json
{
  "notebook_path": "/abs/path/to/nb.ipynb",
  "cell_id":      "alpha",            // UUID-style id, or `cell-N` numeric fallback
  "new_source":   "x = 1",
  "cell_type":    "code",             // optional for replace, required for insert
  "edit_mode":    "replace"           // replace | insert | delete (default: replace)
}
```

| Edit mode | Behaviour |
|-----------|-----------|
| `replace` | Overwrite `cells[i].source`. Code cells get `execution_count: null` and `outputs: []` (the diff stays sane). Markdown cells preserve all metadata. |
| `insert`  | Add a new cell AFTER the anchor `cell_id`. Empty `cell_id` inserts at position 0. `cell_type` required. nbformat ≥ 4.5 gets a fresh 12-char base-36 id. |
| `delete`  | Remove the cell at `cell_id`. |

`cell_id` resolution: literal `cells[i].id` match first; falls back
to `cell-N` numeric index (matches the leak's `parseCellId`).
Failure lists up to 10 available ids in the error message.

## Defensive behaviour

- **Absolute path required.** Refuses relative paths to avoid
  ambiguity across daemon cwd changes.
- **`.ipynb` extension required.** Other file types fall to
  `FileEdit`.
- **Replace at end-of-cells auto-converts to insert.** Lift from
  the leak (`NotebookEditTool.ts:372-377`). Requires `cell_type` in
  that path.
- **Bad writes leave the file untouched.** Validation runs before
  write; any error returns before `std::fs::write`.

## Plan-mode classification

Classified `FileEdit` (mutating) in
`nexo_core::plan_mode::MUTATING_TOOLS`. Plan-mode-on goals get a
`PlanModeRefusal` rather than a silent edit.

## Output

```json
{
  "notebook_path": "/abs/path/...",
  "edit_mode": "replace" | "insert" | "delete",
  "cell_id": "alpha",
  "cell_type": "code" | "markdown",
  "language": "python",        // from notebook.metadata.language_info.name
  "total_cells": 3,
  "cells_delta": 1             // -1 for delete, +1 for insert, 0 for replace
}
```

## Out of scope (deferred)

- **Read-before-Edit guard.** The leak requires `Read` to have been
  called on the file in the same session before `NotebookEdit` is
  allowed. Nexo-rs does not have a shared file-state cache yet — the
  guard becomes useful when Phase 67 driver-loop adds one.
- **Attribution / file-history tracking.** The leak's
  `fileHistoryTrackEdit` records edits to a per-session ledger.
  Skipped — the workspace-git layer (Phase 10.9) covers the same
  use case.
- **Multi-cell batch edits.** One operation per call by design;
  callers loop over cell ids.

## References

- **PRIMARY**:
  `claude-code-leak/src/tools/NotebookEditTool/NotebookEditTool.ts:30-489`,
  `claude-code-leak/src/utils/notebook.ts::parseCellId`.
- **SECONDARY**: OpenClaw `research/` — no equivalent
  (`grep -rln "ipynb|jupyter|nbformat" research/src/` returns
  nothing).
- Plan + spec: `proyecto/PHASES.md::79.13`.
