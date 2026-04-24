# ADR 0002 — NATS as the broker

**Status:** Accepted
**Date:** 2026-01

## Context

The event bus sits under every inter-plugin and inter-agent
communication. Requirements:

- Subject-based routing with wildcards (`plugin.inbound.*`,
  `agent.route.<id>`)
- Low-latency pub/sub (sub-millisecond on LAN)
- No broker-side state to manage unless we opt in
- Clustered production deployments
- Mature async Rust client

Alternatives considered:

- **RabbitMQ** — heavier, queue-per-binding mental model fits less
  well for fan-out across plugin instances, ops overhead higher
- **Redis streams / pub-sub** — streams are great for durable event
  logs but the stream-per-subject model clashes with free-form
  `plugin.outbound.<channel>.<instance>` naming; pub-sub has no
  durability
- **Kafka** — overkill for sub-millisecond request/reply loops,
  heavy ops, partition count becomes a thing you think about
- **Custom over TCP** — too much invented complexity

Additional implementation note: a crate literally called `natsio`
came up in early design research; it does not exist on crates.io.
The real Rust client is `async-nats` (from the NATS org itself),
matching the NATS 2.10 server line.

## Decision

Use NATS as the broker. Specifically:

- **Client:** `async-nats = "0.35"` (pinned in `Cargo.toml`)
- **Subject namespace:** `plugin.inbound.*`, `plugin.outbound.*`,
  `plugin.health.*`, `agent.events.*`, `agent.route.*`
- **Fallback:** a local `tokio::mpsc` bus implementing the same
  `Broker` trait for offline / single-machine runs
- **Durability:** SQLite disk queue in front of every publish;
  drains FIFO on reconnect; 3 attempts before DLQ

## Consequences

**Positive**

- Standard ops path (monitor on `:8222/healthz`, prometheus exporter,
  clustering via well-known recipes)
- Pub/sub semantics are trivial to reason about
- Swapping in JetStream later for persistent streams is additive
- Zero broker state in the happy path — restart NATS without
  catastrophe thanks to the disk queue

**Negative**

- NATS auth (NKey / JWT) has its own learning curve — see the
  [NATS TLS + auth recipe](../recipes/nats-tls-auth.md)
- No built-in message ordering guarantee across subjects (only
  per-subscriber). Callers that need ordering (e.g. delegation with
  correlation id) must enforce it themselves

**Forbidden anti-pattern**

- **Do not** use `natsio` or any other non-async-nats client. The
  crate doesn't exist on crates.io; copy-paste from older design
  docs will mislead.
