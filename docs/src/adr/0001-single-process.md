# ADR 0001 — Single-process runtime over microservices

**Status:** Accepted
**Date:** 2026-01

## Context

nexo-rs hosts N agents, each with its own LLM client, channel
plugins, memory views, and extensions. The natural first instinct for
Rust systems targeting real uptime is to split this into
microservices: an agent service, a plugin service per channel, a
memory service, etc., wired over the broker.

Every microservice adds:

- A serialization boundary (more CPU, more latency)
- A deployment artifact (more Dockerfiles, more CI)
- A failure mode (service down vs process down)
- An ops surface (metrics, health, logs per service)

The alternative — one binary hosting every subsystem as tokio tasks —
gives up none of the durability (the disk queue + DLQ survive a
process restart anyway) and keeps all in-memory caches naturally
shared.

## Decision

Ship **one binary** (`agent`) that hosts:

- Every agent runtime (one tokio task per agent)
- Every channel plugin (WhatsApp, Telegram, browser, …)
- Broker client + disk queue + DLQ
- Memory (short-term in-mem, long-term SQLite, vector sqlite-vec)
- Extension runtimes (stdio / NATS)
- MCP client and server
- TaskFlow runtime
- Metrics + health + admin HTTP servers

Coordination between tasks happens over the broker (NATS or the
local mpsc fallback) exactly as if they were separate processes.
Swapping to microservices later requires zero code changes on either
side of the bus.

## Consequences

**Positive**

- One Dockerfile, one health probe, one metrics endpoint
- No IPC overhead on hot paths (LLM tool calls go
  `ToolRegistry → Extension` through a tokio channel, not a network
  hop)
- Memory caches (session, tool registry) are naturally shared
- Simpler ops: one log stream, one trace span hierarchy

**Negative**

- A bug that panics the process takes down every agent at once (the
  single-instance lockfile mitigates the blast radius by preventing
  silent double-boot)
- Scaling out means running more agent processes pointed at the same
  NATS — isolation between them requires deliberate NATS subject
  partitioning

**Escape hatch**

If a subsystem needs its own lifecycle (example: a GPU-heavy
inference service), ship it as a **NATS extension** — it's
automatically out-of-process and auto-discovered by the agent.
Microservices by the back door, without splitting the monolith first.
