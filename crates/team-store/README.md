# nexo-team-store

Phase 79.6 — SQLite-backed registry + audit log for multi-agent teams.

Three tables (`teams`, `team_members`, `team_events`) consumed by the
five `Team*` tools in `nexo-core`:

| Tool | Op kind |
|------|---------|
| `TeamCreate` | mutating |
| `TeamDelete` | mutating |
| `TeamSendMessage` | mutating |
| `TeamList` | read-only |
| `TeamStatus` | read-only |

See `docs/src/architecture/team-tools.md` for the full surface and
`PHASES.md::79.6` for the implementation spec.
