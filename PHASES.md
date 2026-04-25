# Phases

This file is the **English roadmap index**.

Historical phase-by-phase detailed notes that were previously written in Spanish are preserved at:
- `archive/spanish/PHASES.es.txt`

## Status

All planned implementation phases are complete through Phase 19.

## Completed roadmap

1. Phase 1 — Core Runtime
2. Phase 2 — NATS Broker
3. Phase 3 — LLM Integration
4. Phase 4 — Browser CDP
5. Phase 5 — Memory System
6. Phase 6 — WhatsApp Plugin
7. Phase 7 — Heartbeat Scheduler
8. Phase 8 — Agent-to-Agent Delegation
9. Phase 9 — Production Polish
10. Phase 10 — Soul, Identity, Learning
11. Phase 11 — Extension System
12. Phase 12 — MCP Support
13. Phase 13 — Skills and External Integrations
14. Phase 14 — TaskFlow Runtime
15. Phase 15 — Claude Subscription Auth
16. Phase 16 — Per-Binding Capability Override
17. Phase 17 — Per-Agent Credentials
18. Phase 18 — Config Hot Reload
19. Phase 19 — Generic Poller Subsystem

## Current working mode

- New work is tracked as follow-ups and hardening tasks.
- Active backlog lives in `FOLLOWUPS.md`.
- Architecture remains documented in `design-agent-framework.md`.

## How to add a new phase

When a new major implementation phase is introduced:

1. Add the phase title and objective here.
2. Add explicit done criteria.
3. Add the implementation checklist in English.
4. Link follow-up debt entries in `FOLLOWUPS.md`.
5. Update mdBook docs in the same commit.

## Documentation policy

- Keep Markdown documentation in English.
- Keep historical non-English material only in `archive/spanish/*.txt`.
