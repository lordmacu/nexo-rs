# Architecture Decision Records

Short documents capturing **why** the architecture is the way it is.
Each ADR names an alternative that was considered and rejected, and
the forces that drove the choice. Read these when you're tempted to
change something load-bearing.

Format loosely follows Michael Nygard's ADR template: context,
decision, consequences.

## Index

| # | Title | Status |
|---|-------|--------|
| [0001](./0001-single-process.md) | Single-process runtime over microservices | Accepted |
| [0002](./0002-nats-broker.md) | NATS as the broker | Accepted |
| [0003](./0003-sqlite-vec.md) | sqlite-vec for vector search | Accepted |
| [0004](./0004-per-agent-sandbox.md) | Per-agent tool sandboxing at registry build time | Accepted |
| [0005](./0005-drop-in-agents.md) | Drop-in `agents.d/` directory for private configs | Accepted |
| [0006](./0006-workspace-git.md) | Per-agent git repo for memory forensics | Accepted |
| [0007](./0007-whatsapp-signal-protocol.md) | WhatsApp via whatsapp-rs (Signal Protocol) | Accepted |
| [0008](./0008-mcp-dual-role.md) | MCP dual role — client and server | Accepted |
| [0009](./0009-dual-license.md) | Dual MIT / Apache-2.0 licensing | Accepted |

## Writing a new ADR

1. Copy the template (next ADR below, or use `0001` as a reference)
2. Number sequentially: `NNNN-short-slug.md`
3. Set `status: Proposed` while in review, flip to `Accepted` or
   `Rejected` after the discussion settles
4. Link from this index
5. **Do not edit accepted ADRs in place.** Create a new ADR that
   supersedes it and mark the old one `Superseded by NNNN`.

ADRs are load-bearing documentation — they're how future you (and
future contributors) learn that "NATS over RabbitMQ was not an
accident."
