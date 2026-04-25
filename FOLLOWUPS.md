# Follow-ups

This file tracks the **active technical backlog** in English.

Historical detailed notes that were previously written in Spanish are preserved at:
- `archive/spanish/FOLLOWUPS.es.txt`

## Rules

- After each `/forge ejecutar`, add any deferred work here.
- Keep each item with: what is missing, why it was deferred, and target phase.
- Move completed items to `Resolved` with a completion date.

## Current status

- Main roadmap phases are completed through Phase 19.
- Active work is now hardening, operational polish, and optional capability expansion.

## Open items

### Security and secret handling

1. **1Password safer execution path is missing (`inject_template` pattern)**
- Missing: operator-controlled command templating to use secrets without reveal.
- Why deferred: requires explicit allowlist and execution policy design.
- Target: secrets hardening.

2. **No local read-audit log for secret access**
- Missing: append-only local audit entries with agent/session context.
- Why deferred: medium-priority compliance/forensics feature.
- Target: security observability pass.

### Memory and transcripts

3. **Session log search has no index (substring scan only)**
- Missing: scalable indexing (likely SQLite FTS) for large transcript sets.
- Why deferred: current workloads are still small/fast enough.
- Target: when transcript volume grows.

4. **Transcript-level sensitive redaction is optional but not implemented**
- Missing: redaction pass for sensitive fields before persistence.
- Why deferred: depends on final secret handling policy and false-positive tolerance.
- Target: privacy hardening.

### Extensions and platform

5. **Skill dependency strict mode is optional and not implemented**
- Missing: strict load mode for missing env/bin requirements.
- Why deferred: current design prefers warn-only startup behavior.
- Target: extension UX hardening.

6. **Version constraints for required binaries are not enforced**
- Missing: `requires.bin_versions` support.
- Why deferred: implementation effort vs low current demand.
- Target: extension manifest evolution.

7. **Some write-capability toggles are env-only (no setup UX yet)**
- Missing: setup/doctor wizard integration for operational toggles.
- Why deferred: functionality exists; UX layer postponed.
- Target: setup polish.

## Resolved (recent highlights)

- Streaming telemetry and streaming runtime wiring completed.
- Per-agent credentials hot-reload completed.
- Browser CDP reliability hardening completed.
- Shared extension resilience helpers extracted.
- Docs sync gate and mdBook English checks enabled.
- 2026-04-25 — SessionLogs tool registered in agent bootstrap and mcp-server (gated on non-empty `transcripts_dir`).
- 2026-04-25 — TaskFlow runtime wiring: shared `FlowManager`, `WaitEngine` tick loop, `taskflow.resume` NATS bridge, and tool actions `wait`/`finish`/`fail` with guardrails (`timer_max_horizon`, non-empty topic+correlation).

## Maintenance note

If a future historical import includes non-English notes, keep them in `archive/spanish/*.txt` and update this Markdown tracker in English only.
