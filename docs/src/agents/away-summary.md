# AWAY_SUMMARY digest

When the user has been silent for a configurable threshold (default
4 hours), the next inbound message triggers a short markdown digest
that summarises everything the agent did during the silence: goals
completed, aborts, failures, and turn counts. Default is **disabled**
— per-binding opt-in.

## Why

In [assistant mode](./assistant-mode.md), the agent runs proactively
in the user's absence. When the user comes back to the chat, they
need a quick recap before the agent processes their new request.
AWAY_SUMMARY is that recap.

The digest answers "what did you do while I was gone?" with a few
counter bullets, NOT a long narrative. If you want a richer
LLM-summarised version (1–3 sentences of natural prose), that's
the deferred 80.14.b follow-up.

## Configuration

```yaml
agents:
  - id: kate
    away_summary:
      enabled: true
      threshold_hours: 4    # default
      max_events: 50        # default
```

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `enabled` | bool | `false` | Master toggle |
| `threshold_hours` | u64 | `4` | Hours of silence before next inbound triggers digest. `0` would fire on every inbound — operator-side rate limiting becomes their responsibility. Rejected at validate time when > 30 days (likely operator confusion). |
| `max_events` | usize | `50` | Cap on events included in the digest. Larger windows still fire but truncate with a `(showing the most recent N — older events may exist)` suffix. Rejected at validate time when 0. |

## Output shape

Template-based markdown (no LLM call in the slim MVP — pure-fn
render):

```
**While you were away** (last 6h12m):
- 7 goal turn(s) recorded
- 4 completed
- 1 aborted/cancelled
- 1 failed
- 1 in progress / other
```

When the event count hits `max_events`, a truncation suffix is
appended:

```
_(showing the most recent 50 — older events may exist)_
```

## Wiring (operator-side)

Today's slim MVP ships the digest helper as a pure-async function
in `nexo-dispatch-tools::away_summary`:

```rust
use nexo_dispatch_tools::try_compose_away_digest;

// Inbound handler — called when a new user message arrives.
let digest = try_compose_away_digest(
    &cfg.away_summary.unwrap_or_default(),
    last_seen,           // Option<DateTime<Utc>> from caller storage
    chrono::Utc::now(),
    turn_log_store.as_ref(),
).await?;

if let Some(text) = digest {
    // Deliver via notify_origin BEFORE processing the user inbound.
    notify_origin(channel, &text).await?;
}

// Atomically update last_seen = now after composing
// (caller-managed storage — pairing store, separate SQLite, in-memory map).
update_last_seen(channel, sender_id, chrono::Utc::now()).await?;

// Now process the user's actual message.
process_inbound(...).await?;
```

The helper walks 4 gates cheapest-first:

1. `cfg.enabled` (opt-in)
2. `last_seen.is_some()` — `None` returns None **without firing**
   so the caller can use it as the bootstrap path (set `last_seen
   = now` without burning the threshold)
3. `now - last_seen >= threshold` — negative elapsed (clock skew)
   returns None
4. Turn-log has at least one event since `last_seen` — empty
   digest is not worth sending

When all four pass, returns `Some(markdown)`.

### Atomic update pattern

The `last_seen` storage is operator-managed in the slim MVP: the
helper accepts the timestamp as a parameter, doesn't couple to
`nexo-pairing` or any specific table. Whatever you choose, update
it atomically AFTER composing the digest so a rapid double-inbound
doesn't fire twice.

## Defense-in-depth

| Edge case | Behaviour |
|-----------|-----------|
| `enabled: false` | Returns None |
| `last_seen: None` (bootstrap) | Returns None — caller sets last_seen without firing |
| `now - last_seen < threshold` | Returns None |
| `now < last_seen` (clock skew) | Returns None |
| Turn-log empty | Returns None |
| `max_events == 0` | Validate rejects at boot (not at runtime) |
| `threshold_hours > 30 days` | Validate rejects at boot |

## Deferred follow-ups

- **80.14.b** — LLM-summarised version: forks a subagent that
  takes the events list and renders a 1–3 sentence prose
  summary. Today's MVP is template-based.
- **80.14.c** — `last_seen_at` tracking in
  `nexo-pairing::PairingStore` with SQLite migration so operators
  don't roll their own.
- **80.14.d** — Per-channel-adapter rendering (whatsapp /
  telegram render markdown differently).
- **80.14.e** — Time-of-day awareness ("don't ping at 3am unless
  awake_hours covers").
- **80.14.f** — Custom prompt template per agent (relevant once
  80.14.b ships).
- **80.14.g** — main.rs inbound interceptor wire (1-line
  invocation site, blocked on dirty-state pattern).

## See also

- [Assistant mode](./assistant-mode.md)
- [Auto-approve dial](./auto-approve.md) — pair with AWAY_SUMMARY
  for the "agent works while user sleeps" workflow
- [Multi-agent coordination](./multi-agent-coordination.md)
