# nexo-project-tracker

The project-state layer of the programmer agent. Parses the
operator's roadmap (`PHASES.md` + `FOLLOWUPS.md`) so Cody can
answer "qué fase va", "qué followups quedan", "show me the body
of 67.10" — and so `program_phase` knows which sub-phase to
dispatch.

This crate is read-only over the markdown files. It never edits
them; that's the operator's job (via `/forge ejecutar`).

## Where it sits

```
nexo-project-tracker     ← THIS crate (no driver deps)
        ↑
nexo-dispatch-tools      ← reads tracker to drive dispatch
        ↑
nexo-core                ← exposes the tracker as a tool
```

## Public surface

### Parser

| Type | Purpose |
|---|---|
| `Phase { id, title, sub_phases }` | One top-level `## Phase NN — Title` block. |
| `SubPhase { id, title, status, body, acceptance }` | One `#### NN.M — Title` heading + the prose between it and the next heading. `status` from trailing emoji (`✅` / `🔄` / `⬜`); `acceptance` parsed out of the body via the recognisable `Acceptance:` markers. |
| `PhaseStatus { Done, InProgress, Pending }` | Matched on emoji. Missing emoji → `Pending`. |
| `FollowUp { code, title, section, status, body }` | One item from FOLLOWUPS.md (`PR-3.`, `H-1.`, etc.). `status` = `Resolved` if title is wrapped in `~~strike~~` or contains `✅`, else `Open`. |
| `parse_phases_file(&Path) / parse_phases_str(&str)` | Top-level parser entry. Honours fenced code blocks (\`\`\` / ~~~) so an embedded `#### ...` inside a code block is not parsed as a heading. |
| `parse_followups_file / parse_followups_str` | Same shape for FOLLOWUPS.md. |

### Acceptance-block recognition (B21)

The parser pulls `acceptance: Vec<String>` out of any of these
shapes inside a sub-phase body:

```text
**Acceptance:**
- cargo build --workspace
- cargo test -p calc parser

### Acceptance
- cargo build
- ./scripts/smoke.sh

Acceptance: `cargo test -p calc`.
```

Multiple Acceptance blocks in one body concatenate. Each bullet
becomes a shell command the dispatch tools turn into an
`AcceptanceCriterion::shell`. The order: caller override →
parsed bullets → workspace defaults (`cargo build --workspace`
+ `cargo test --workspace`).

### Tracker (cached + watched)

| Type | Purpose |
|---|---|
| `ProjectTracker` trait | Async accessors: `phases() / followups() / current_phase() / next_phase() / last_shipped(n) / phase_detail(&str)`. |
| `FsProjectTracker` | Reads `<root>/PHASES.md` (required) and `<root>/FOLLOWUPS.md` (optional) at startup, caches behind a `parking_lot::RwLock` with a 60 s TTL fallback, and starts a `notify` watcher on the parent directory so file edits invalidate the slot. `with_ttl(d) / with_watch()` are the builders. |
| `MutableTracker` | Hot-swappable wrapper over `Box<dyn ProjectTracker>` behind an `ArcSwap`. `switch_to(path)` opens a fresh tracker and replaces the inner Arc; reads stay lock-free. Used by Cody's `set_active_workspace` / `init_project` tools so the operator can switch projects mid-conversation without restarting the daemon. |

### Read tools (`tools.rs`)

`ProjectTracking` bundles a tracker + optional `GitLogReader` +
in-process telemetry counters and exposes:

- `project_status({ kind: current_phase | next_phase |
  phase_detail | followups_open | last_shipped })` — markdown
  output ≤ 4 KiB.
- `project_phases_list({ filter? })` — flat table.
- `followup_detail({ code })` — full body.
- `git_log_for_phase({ phase_id })` — top-N commits matching
  `Phase <id>` in the message; runs through a
  `nexo_resilience::CircuitBreaker` so a stuck git can't take
  down the chat.

### Format helpers

`render_subphase_line / render_subphase_detail / render_phases_table /
render_followups_open / render_followup_detail` plus
`cap_to(s, n)` for byte-bounded output. `DEFAULT_BYTE_CAP =
3500` leaves headroom under Telegram / WhatsApp's 4 KiB limit.

### Config

`ProjectTrackerConfig { tracker, program_phase, agent_registry }`
deserialises `config/project-tracker/project_tracker.yaml`. The
program_phase + agent_registry blocks are consumed by
`nexo-dispatch-tools` and the boot helper in `src/main.rs`.

## What's NOT here

- Editing PHASES.md / FOLLOWUPS.md. The operator's `/forge
  ejecutar` flow owns commits to those files; the tracker
  picks the new state up via the file watcher.
- Persistence of parsed state. Everything is regenerated from
  the markdown on demand; `notify` invalidations + TTL keep the
  cache honest.

## See also

- `architecture/project-tracker.md` — programmer agent overview.
- `proyecto/PHASES.md` — the canonical fixture this parser was
  built against.
