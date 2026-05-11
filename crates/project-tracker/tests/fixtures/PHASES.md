# Phases — test fixture

A trimmed, synthetic `PHASES.md` used **only** by
`nexo-project-tracker`'s tests — it is not the project roadmap. It
carries just enough phases, sub-phases and status glyphs to exercise
the parser and the read tools (`current_phase`, `phase_detail`,
`last_shipped`, `project_phases_list`). The dialect mirrors the real
file: `## Phase N — title`, `#### N.M — title  <glyph>`, where the
trailing glyph is `✅` (Done), `🔄` (InProgress) or `⬜`/absent
(Pending).

## Phase 1 — Core runtime

#### 1.1 — Event bus + session manager  ✅
#### 1.2 — Circuit breaker + heartbeat  ✅

## Phase 67 — Driver subsystem

#### 67.0 — AgentHarness trait + Goal/Attempt types  ✅
#### 67.1 — claude_cli skill (spawn + stream-json)  ✅
#### 67.2 — Session-binding store (SQLite)  ✅
#### 67.3 — MCP permission_prompt in-process  ✅
#### 67.4 — Driver agent loop + budget guards  ✅
#### 67.5 — Acceptance evaluator (cargo + verifiers)  ✅
#### 67.6 — Git worktree sandboxing + checkpoints  ✅
#### 67.7 — Semantic decision memory (vector recall)  ✅
#### 67.8 — Replay policy (resume after a mid-turn crash)  ✅
#### 67.9 — Opportunistic compaction  ✅
#### 67.10 — Escalation to WhatsApp / Telegram  ⬜
#### 67.11 — Shadow mode (calibrate before auto)  ⬜
#### 67.12 — Parallel multi-goal  ⬜
#### 67.13 — Cost dashboard + admin-ui tile  ⬜
