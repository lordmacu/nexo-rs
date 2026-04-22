# Implement Phase

Implement the specified phase from the design doc: $ARGUMENTS

Reference: `proyecto/design-agent-framework.md` → "Implementation Phases"

Phases:
1. Core runtime + local broker + config loading
2. NATS integration + persistent queue + circuit breaker
3. MiniMax LLM client + rate limiter + tool calling
4. Browser CDP plugin
5. Memory (SQLite + sqlite-vec)
6. WhatsApp plugin (wrap ../whatsapp-rs)
7. Heartbeat scheduler
8. Agent-to-agent routing
9. Observability + health checks + Docker Compose

Before writing code: read CLAUDE.md and the relevant section of the design doc.
