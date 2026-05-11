# Follow-ups — test fixture

A trimmed, synthetic `FOLLOWUPS.md` used **only** by
`nexo-project-tracker`'s tests — not the real backlog. It carries one
section with a couple of resolved items (`~~**title**~~  ✅`) and one
open item (plain `**title**`) so the parser, the open/resolved split
and `followup_detail` have something to chew on.

### Phase 26 — Pairing protocol

PR-1. ~~**Plugin gate hooks for WhatsApp + Telegram**~~  ✅ shipped
- Wired the inbound gate into both channel intakes.

PR-1.1. ~~**Challenge reply through channel adapter**~~  ✅ shipped
- Adapter round-trips the pairing challenge end to end.

PR-3. **`tunnel.url` integration in URL resolver**
- Missing: thread the resolved tunnel URL through the URL resolver.
- Why deferred: blocked on the resolver refactor landing first.
