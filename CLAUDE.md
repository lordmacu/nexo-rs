# Agent Framework — Project Guide

Architecture, workspace layout, key decisions, retry policy, fault-tolerance
rules, agent-to-agent comms: see [`/home/familia/chat/CLAUDE.md`](../CLAUDE.md)
(root guide). This file holds **project-specific rules + active phase
tracker**.

## Active phases

| Phase | Name | Sub-phases | Status |
|-------|------|-----------|--------|
| 80 | Autonomous assistant mode | 25/22 | 🔄 follow-ups open (80.7 DEFER, 80.13 ❌ DROPPED) |
| 81 | Plug-and-play plugin system | 34/34 | 🔄 (81.3/81.4/81.5/81.6/81.7/81.8/81.9/81.9.b/81.10/81.11/81.12.a-d/81.13 manifest unification/81.14/81.14.b/81.15.a-c.b/81.16/81.17/81.17.b/81.17.c/81.18/81.18.b.1 telegram subprocess flip/81.18.b.2 whatsapp flip + pairing bridge/81.19.a/81.19.b email extract + factory flip/81.20.a-b.c/81.21/81.21.b/81.22/81.23/81.24/81.25/81.26/81.27/81.28/81.29 ✅; 81.12.e DEFER → 81.17; 81.20.c typing presence RPC/81.21.b.b/81.21.c pending) |
| 82 | Multi-tenant SaaS extension enablement | 15/15 | ✅ shipped 2026-05-01 (follow-ups in FOLLOWUPS.md) |
| 83 | Microapp framework foundation | 13/17 | 🔄 (83.3/83.4/83.5/83.6/83.7/83.8/83.11/83.15/83.16/83.17 ✅; 83.14 publish-readiness ✅ — execution pending; 83.1/83.2/83.9/83.10/83.12/83.13 pending — heavy product/UI sub-phases) |
| 84 | Coordinator agent persona + worker continuation | 5/5 | ✅ shipped 2026-05-01 (follow-ups in FOLLOWUPS.md) |
| 85 | Compaction hardening: reactive recovery + cache-aware micro-compact | 2/2 | ✅ shipped 2026-05-01 (follow-ups in FOLLOWUPS.md) |
| 86 | Memory observability | 1/1 | ✅ shipped 2026-05-01 (86.2 ❌ DROPPED, fire-site wiring in FOLLOWUPS) |
| 87 | LLM-as-judge verifier (+ container runtime DEFER) | 1/1 | ✅ shipped 2026-05-01 (87.2 DEFER until Phase 32/82 hardening; backend wire-up in FOLLOWUPS) |
| 88 | WhatsApp recording-presence indicator (`media="audio"`) | 4/4 | ✅ shipped 2026-05-07 (88.1 wire shape + 88.2 wa-agent runtime + 88.3 plugin wiring + 88.4 docs/follow-ups; 6 deferreds in FOLLOWUPS.md) |
| 89 | Locale-aware agent language (BCP-47) + per-locale addendum + voice picker | 5/5 | ✅ shipped 2026-05-07 (89.1 SDK Locale + addenda + voice picker + BUG FIX (legacy `es` → voseo) + 89.2 admin RPC validation + 89.3 microapp consumes SDK + 89.4 UI grouped dropdown + 89.5 docs/follow-ups; 6 deferreds in FOLLOWUPS.md) |

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

## Build toolchain

Machine-wide config in `~/.cargo/config.toml`: `mold` linker via
`clang`, `sccache` as `rustc-wrapper`, `debug = "line-tables-only"`
on dev. Don't `cargo install` anything that overrides those.

Profiles defined in this workspace's `Cargo.toml`:

- `release` — production: `lto = "thin"`, `codegen-units = 1`.
- `release-fast` — same opt-level, no LTO, codegen-units=16. Use
  for local validation; reserve `--release` for publish.
- `dist` — what `cargo dist` ships.

Use `cargo nextest run` instead of `cargo test` (parallel, faster).
Inspect cache hits with `sccache --show-stats`.

## What NOT to do

- Don't hardcode API keys — use `${ENV_VAR}` in YAML
- Don't use `natsio` crate — use `async-nats`
- Don't skip circuit breaker on external calls
- Don't commit anything in `secrets/`
- Don't write Spanish in code identifiers or comments
