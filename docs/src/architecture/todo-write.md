# `TodoWrite` (Phase 79.4)

`TodoWrite` is the model's intra-turn scratch list. Every call
replaces the entire list (full-replace semantics). The runtime wipes
the stored list to `[]` whenever every item is `completed`, so the
next planning cycle starts fresh.

The tool is always callable — including while plan mode is on, since
it never touches workspace, broker, or external state. Lift from
`claude-code-leak/src/tools/TodoWriteTool/TodoWriteTool.ts:31-115`.

## Diff vs Phase 14 TaskFlow

| Trait | `TodoWrite` (this) | TaskFlow (Phase 14) |
|-------|--------------------|---------------------|
| Lifetime | Per goal, in-memory | Persistent, cross-session |
| Owner | Model | Operator + model + flows |
| Shape | Flat array | DAG with deps + waits |
| Semantics | Full-replace, wipe-on-all-completed | Partial mutations, manual close |
| Survives daemon restart | No | Yes |
| When to use | Coordinating sub-steps inside a long Phase 67 driver-loop turn without spawning sub-goals | Multi-day work programs, cross-session state |

## Tool shape

```json
{
  "todos": [
    {
      "content": "Run cargo test",
      "status": "pending",
      "activeForm": "Running cargo test"
    }
  ]
}
```

Every item must carry both `content` (imperative) and `activeForm`
(present continuous) — the leak shows `activeForm` is what dashboards
render while the item is in_progress, so they get a natural progress
string without grammar fixup. Snake-case `active_form` is also
accepted for consistency with the rest of nexo-rs.

`status` is one of `pending | in_progress | completed`.

## Bounds

- Max 50 items per goal (defensive — the leak does not enforce one;
  nexo-rs adds the cap so a runaway model cannot grow the list
  unbounded).
- Max 200 UTF-8 bytes per `content` and `active_form` field.
- A bad write rejects without clobbering the existing list.

## Response

```json
{
  "old_todos": [...],
  "new_todos": [...],
  "wiped_on_all_completed": false,
  "in_progress_count": 1,
  "instructions": "Todos updated. Keep exactly one item `in_progress` at a time. Mark completed IMMEDIATELY after finishing each task; do not batch completions."
}
```

`old_todos` echoes the previous list so the model sees the diff in
the same turn. `wiped_on_all_completed: true` flags that the runtime
just cleared the stored list.

## When the model should use it

The tool description ships the canonical "use proactively for 3+
step tasks" guidance lifted from
`claude-code-leak/src/tools/TodoWriteTool/prompt.ts`. In short:
multi-step coding tasks → seed a list, mark exactly one item
`in_progress`, mark it `completed` the moment it finishes (don't
batch), tear the list down once everything is done.

## References

- **PRIMARY**: `claude-code-leak/src/tools/TodoWriteTool/TodoWriteTool.ts:31-115`
  + `claude-code-leak/src/utils/todo/types.ts:1-19`
  + `claude-code-leak/src/tools/TodoWriteTool/prompt.ts` (guidance).
- **SECONDARY**: OpenClaw `research/` — no equivalent
  (`grep -rln "todo" research/src/` returns only unrelated cron /
  delivery files).
