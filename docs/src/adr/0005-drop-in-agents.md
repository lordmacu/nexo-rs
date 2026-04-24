# ADR 0005 — Drop-in `agents.d/` directory for private configs

**Status:** Accepted
**Date:** 2026-02

## Context

Two kinds of agent content coexist in the same project:

- **Public** — the framework demo agents, ops helpers, templates
- **Private** — sales prompts, tarifarios, internal phone numbers,
  compliance-flagged customer scripts

The obvious "one `agents.yaml`" approach forces everything to be
either committed (leaking business content) or gitignored (losing
the template reference). Neither is acceptable.

## Decision

Split by **path convention**:

- `config/agents.yaml` — committed, public-safe defaults
- `config/agents.d/*.yaml` — **gitignored** drop-in directory
- `config/agents.d/*.example.yaml` — committed templates
- Merge happens at load time: every `.yaml` in `agents.d/` gets its
  `agents:` array concatenated to the base list
- Files load in **lexicographic filename order**, so `00-common.yaml`
  + `10-prod.yaml` composes predictably
- `.gitignore` includes:
  ```
  config/agents.d/*.yaml
  !config/agents.d/*.example.yaml
  ```

## Consequences

**Positive**

- Safe to open-source the repo; real business content stays private
- Templates stay in git (`ana.example.yaml`) so newcomers can copy
  and fill
- Per-environment layering falls out for free (`00-dev.yaml` vs
  `10-prod.yaml` per deploy)

**Negative**

- Agent-id collisions across files are possible — the loader rejects
  them at startup with an explicit error. Operators must coordinate
  file naming
- Not every config is split this way — some operators expected
  `plugins.d/`, `llm.d/`, etc. We decided against the generalization
  until a concrete need appeared

## Related

- [Config — drop-in agents](../config/drop-in.md) — full mechanics
- [Recipes — WhatsApp sales agent](../recipes/whatsapp-sales-agent.md) —
  shows the pattern in practice
