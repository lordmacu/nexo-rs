# ADR 0006 — Per-agent git repo for memory forensics

**Status:** Accepted
**Date:** 2026-03

## Context

An agent's memory evolves over time — dream sweeps promote
memories, the agent writes USER.md / AGENTS.md / SOUL.md revisions,
session closes append to MEMORY.md. When an agent misbehaves, "what
did it know and when?" is a real debugging question.

Options considered:

- **Append-only audit log per write** — possible, but rolls out a
  custom scheme for every file
- **DB-level revision history** — works for LTM rows but not for
  workspace markdown files
- **Git** — battle-tested, standard tooling, `git log` and
  `git blame` ship with every developer's laptop

## Decision

When `workspace_git.enabled: true`, the agent's `workspace`
directory is a **per-agent git repository**. The runtime commits at
three specific moments:

- **Dream sweep finishes** — commit subject `promote`, body lists
  promoted memories with scores
- **Session close** — commit subject `session-close`, body includes
  session id and agent id
- **Explicit `forge_memory_checkpoint(note)` tool call** — commit
  subject `checkpoint: {note}`

Commit mechanics:

- Staged: every non-ignored file (respects auto-generated
  `.gitignore` that excludes `transcripts/`, `media/`, `*.tmp`)
- Skipped: files larger than 1 MiB (`MAX_COMMIT_FILE_BYTES`)
- Idempotent: no-op commit if tree clean
- Author: `{agent_id} <agent@localhost>` (configurable)
- No remote by default — operators add one if archival matters

## Consequences

**Positive**

- `git log` gives you a timestamped history of every memory
  evolution, for free
- `memory_history` tool lets the LLM reason about its own past
  state — e.g. "what did I believe about this user last week?"
- `git diff <oldest>..HEAD` is one command away when debugging
- Familiar tooling for humans (`git bisect` a misbehaving agent)

**Negative**

- Repositories grow over time; operators should add a remote with
  periodic push-and-repack
- Commits are process-scoped — an agent process crash between "write
  MEMORY.md" and "commit" leaves an uncommitted diff. The next
  commit picks it up, but at that point the audit event is merged
- Transcripts are **intentionally excluded** from commits — they
  can be enormous and aren't the forensic artifact the ADR is aimed at

## Related

- [Soul — MEMORY.md + workspace-git](../soul/memory.md#workspace-git-phase-109)
- [Agent runtime — Graceful shutdown](../architecture/agent-runtime.md#graceful-shutdown)
  (session-close commit runs here)
