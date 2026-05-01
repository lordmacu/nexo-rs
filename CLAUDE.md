# Agent Framework — Project Guide

Architecture, workspace layout, key decisions, retry policy, fault-tolerance
rules, agent-to-agent comms: see [`/home/familia/chat/CLAUDE.md`](../CLAUDE.md)
(root guide). This file holds **project-specific rules + active phase
tracker**.

## Active phases

| Phase | Name | Sub-phases | Status |
|-------|------|-----------|--------|
| 80 | Autonomous assistant mode | 25/22 | 🔄 follow-ups open (80.7 DEFER, 80.13 ❌ DROPPED) |
| 81 | Plug-and-play plugin system | 2/13 | 🔄 (81.13 DEFER) |
| 82 | Multi-tenant SaaS extension enablement | 15/15 | ✅ shipped 2026-05-01 (follow-ups in FOLLOWUPS.md) |
| 83 | Microapp framework foundation | 1/17 | 🔄 (3 new sub-phases: 83.15 testing harness, 83.16 error→operator, 83.17 config schema validation) |
| 84 | Coordinator agent persona + worker continuation | 5/5 | ✅ shipped 2026-05-01 (follow-ups in FOLLOWUPS.md) |
| 85 | Compaction hardening: reactive recovery + cache-aware micro-compact | 2/2 | ✅ shipped 2026-05-01 (follow-ups in FOLLOWUPS.md) |
| 86 | Memory observability | 1/1 | ✅ shipped 2026-05-01 (86.2 ❌ DROPPED, fire-site wiring in FOLLOWUPS) |
| 87 | LLM-as-judge verifier (+ container runtime DEFER) | 0/1 | ⬜ AFTER-PHASE-84 (87.2 DEFER until Phase 32 or 82 hardening) |

**Curation 2026-05-01** (single source of truth for active scope):
[`PHASES-curated.md`](PHASES-curated.md) — what is active, what was
dropped/deferred, and why.

- **Detail per sub-phase** → [`PHASES.md`](PHASES.md)
- **Open follow-ups** (deferred items, deferreds against closed phases) → [`FOLLOWUPS.md`](FOLLOWUPS.md)
- **Closed phases** → [`PHASES-archive.md`](PHASES-archive.md)
- **Backlog phases** (not yet active) → [`PHASES-archive.md`](PHASES-archive.md) § Backlog

## Mandatory rules

1. **Forge flow**: every feature follows `brainstorm → spec → plan → ejecutar`. Never skip.
2. **Brainstorm-mining**: each `/forge brainstorm|spec|plan` cites ≥ 1 `path:line` from `research/` (OpenClaw) and any local reference repositories. Absence must be explicitly stated.
3. **Progress tracking**: when a sub-phase ships, mark `[x]` in `PHASES.md` and update the active table above. When all sub-phases ✅, move the row to `PHASES-archive.md` in the same commit.
4. **admin-ui sync**: operator-visible knob → checkbox in `admin-ui/PHASES.md` (same commit). Orthogonal items go in the tech-debt registry section.
5. **Capability inventory**: new env toggle that gates dangerous behavior (`*_ALLOW_*`, `*_REVEAL`, `*_PURGE`, allowlists) → `crates/setup/src/capabilities.rs::INVENTORY` entry (same commit). Without it `agent doctor capabilities` is silently incomplete.
6. **Docs sync**: any user-visible change (config field, YAML key, env var, CLI flag, plugin/extension API, behavior, retry policy, public type) → `docs/src/` page updated and `mdbook build docs` clean (same commit). Pure-internal refactors are exempt — note that in the commit body.
7. **Language**: code identifiers + comments + repo Markdown in English. Conversations with Cristian in Spanish.

## Forge skills

| Command | When |
|---------|------|
| `/forge brainstorm <topic>` | New feature — explore + mine references |
| `/forge spec <topic>` | After brainstorm approval — define interfaces, config, edge cases |
| `/forge plan <topic>` | After spec approval — atomic steps with done criteria |
| `/forge ejecutar <topic>` | After plan approval — implement, `cargo build` after each step |

Coding for any sub-phase auto-runs `/forge ejecutar`.

## OpenClaw reference

Location: `research/` — TypeScript, single-process, Node 22+. Reference, not template.

| Path | Mine for |
|------|----------|
| `research/src/agents/` | agent loop patterns |
| `research/src/channels/` | channel/plugin interface contracts |
| `research/extensions/` | plugin implementations (whatsapp → `extensions/wacli/`, browser → `extensions/canvas/`) |
| `research/src/memory-host-sdk/` | memory architecture |
| `research/docs/` | design decisions |

## What NOT to do

- Don't hardcode API keys — use `${ENV_VAR}` in YAML
- Don't use `natsio` crate — use `async-nats`
- Don't skip circuit breaker on external calls
- Don't commit anything in `secrets/`
- Don't write Spanish in code identifiers or comments
