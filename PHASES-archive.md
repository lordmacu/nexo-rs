# Closed phases — append-only ledger

Phases listed here are **fully checked off** in the sub-phase table
(M/M ✅). Open follow-ups against a closed phase are tracked in
`FOLLOWUPS.md`, not here — closing the table doesn't require zero
follow-ups, only that no un-checked sub-phase remains.

Authoritative detail (sub-phase done criteria, design notes, deferred lists)
lives in `PHASES.md` and `FOLLOWUPS.md`. Active work tracked in
`proyecto/CLAUDE.md` + `PHASES.md`.

If a deferred item resurfaces against a closed phase, **do not edit the row
here** — reopen the phase by adding a row to the active table in
`CLAUDE.md` with a `🔄 reopened` marker, and document the new sub-phase in
`PHASES.md`.

## Closed phases

| Phase | Name | Sub-phases | Closed |
|-------|------|-----------|--------|
| 1 | Core Runtime | 7/7 | ✅ |
| 2 | NATS Broker | 6/6 | ✅ |
| 3 | LLM Integration | 6/6 | ✅ |
| 4 | Browser CDP | 6/6 | ✅ |
| 5 | Memory | 5/5 | ✅ |
| 6 | WhatsApp Plugin | 9/9 | ✅ |
| 7 | Heartbeat | 3/3 | ✅ |
| 8 | Agent-to-Agent | 3/3 | ✅ |
| 9 | Polish | 6/6 | ✅ |
| 10 | Soul, Identity & Learning | 9/9 | ✅ |
| 11 | Extension System | 8/8 | ✅ |
| 12 | MCP Support | 8/8 | ✅ |
| 13 | Skills (OpenClaw + Google + infra) | 22/22 | ✅ |
| 14 | TaskFlow runtime | 7/7 | ✅ |
| 15 | Claude subscription auth | 9/9 | ✅ |
| 16 | Per-binding capability override | 7/7 | ✅ |
| 17 | Per-agent credentials (WA/TG/Google) | 11/11 | ✅ |
| 18 | Config hot-reload | 9/9 | ✅ |
| 20 | `agent_turn` poller | 4/4 | ✅ |
| 70 | Pairing/Dispatch DX cleanup | 8/8 | ✅ |
| 71 | Agent registry persistence + shutdown drain | 5/5 | ✅ |
| 72 | Turn-level audit log | 5/5 | ✅ |
| 73 | MCP wire fixes | 8/8 | ✅ |
| 74 | MCP conformance | 3/3 | ✅ |
| 75 | Acceptance autodetect | 3/3 | ✅ |
| 76 | MCP server hardening | 16/16 | ✅ |
| 77 | Tool parity sweep | 20/20 | ✅ |
| 79 | Tool surface parity sweep | 14/14 | ✅ |
| 31 | Plugin marketplace + multi-language authoring | 13/13 | ✅ shipped 2026-05-04 |

## Backlog phases (not yet active, not yet closed)

These have entries in `PHASES.md` but no implementation work has started.
Pulled out of the active CLAUDE.md table to keep that table focused on
phases currently being shipped. Move back to active when work begins.

- Phase 19 — Pollers V2 backlog
- Phase 21 — Link understanding (open follow-ups)
- Phase 22 — Slack/Discord plugins
- Phase 23 — Realtime voice
- Phase 24 — Image generation
- Phase 25 — `web_search` (open follow-ups)
- Phase 26 — Pairing + per-channel reply adapters (open follow-ups)
- Phase 27 — Release packaging (Tier 1 + Tier 3 ✅; Tier 2 = 27.4.b open)
- Phase 32 — Multi-host orchestration (deferred)
- Phase 46 — Local LLM as primary agent provider
- Phase 51 — Eval harness
- Phase 67 — Claude Code self-driving agent (67.0–67.H ✅; 67.10–67.13 backlog)
- Phase 68 — Local LLM tier (15 sub-phases backlogged)
- Phase 69 — Setup wizard agent-centric submenu (✅ shipped — verify before re-archiving)
- Phase 78 — _Reserved_

## Maintenance

When closing a phase:

1. Confirm `FOLLOWUPS.md` has zero open items mentioning the phase number.
2. Move its row from `CLAUDE.md` table to the table above.
3. Append a one-line entry to `FOLLOWUPS.md` § "Resolved (recent
   highlights)" referencing this archive.
4. Do not delete from `PHASES.md` — that file is the authoritative
   sub-phase ledger and stays untouched.
