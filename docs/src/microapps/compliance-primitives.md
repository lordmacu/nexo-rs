# Compliance primitives — when to use which

`nexo-compliance-primitives` (Phase 83.5) ships six reusable
primitives every conversational microapp needs. This page maps
each primitive to the decision it makes and the symptom that
tells you to wire it in.

| Primitive | Decision | Symptom that demands it |
|---|---|---|
| `AntiLoopDetector` | Block when same body N+ times in window OR auto-reply signature seen | Bot is talking to a bot; "Recibido / Mensaje automático" replies bouncing |
| `AntiManipulationMatcher` | Block on prompt-injection / role-hijack | "Ignore previous instructions", "Act as", system-prompt extraction |
| `OptOutMatcher` | Block + `do_not_reply_again` on opt-out keyword | User says "STOP", "no me escribas más", "unsubscribe" |
| `PiiRedactor` | Transform: strip PII before LLM sees text | Compliance / data-handling rules forbid passing card / phone / email through LLM |
| `RateLimitPerUser` | Block on bucket exhausted | One user spamming; protect downstream from runaway senders |
| `ConsentTracker` | Gate outbound: only send when `OptedIn` | GDPR / CAN-SPAM / WhatsApp Business cold-outbound rules |

All six plug into the Phase 83.3 hook interceptor. The microapp
casts the verdict into a `HookOutcome::{Block, Transform,
Continue}` and the daemon acts.

## AntiLoopDetector — anti-loop

**When to wire it:**

- Channel is bidirectional and the user-agent could be another
  bot (WhatsApp Business with auto-replies, email
  out-of-office responders, etc.).
- You see your own bot's messages echoed back in the inbound
  feed.

**Wire shape:**

```rust
let mut detector = AntiLoopDetector::new(3, Duration::from_secs(300));

async fn before_message(args: Value, ctx: HookCtx) -> Result<HookOutcome, ToolError> {
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    match DETECTOR.lock().unwrap().record_and_evaluate(body) {
        LoopVerdict::Repetition { count } => Ok(HookOutcome::Block {
            reason: format!("loop: same message {count}× in 5 min"),
            do_not_reply_again: true,
        }),
        LoopVerdict::AutoReplySignature { phrase } => Ok(HookOutcome::Block {
            reason: format!("auto-reply signature: `{phrase}`"),
            do_not_reply_again: true,
        }),
        LoopVerdict::Clear => Ok(HookOutcome::Continue),
    }
}
```

**Tunables:** threshold (count to trip) + window (rolling
duration). Custom signature lists via `with_signatures(...)`.

## AntiManipulationMatcher — prompt-injection

**When to wire it:**

- User-controlled inbound text reaches the LLM verbatim.
- Compliance rules prohibit roles or instruction overrides.

**Wire shape:**

```rust
let m = AntiManipulationMatcher::default();

async fn before_message(args: Value, _ctx: HookCtx) -> Result<HookOutcome, ToolError> {
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    match m.evaluate(body) {
        ManipulationVerdict::Matched { phrase } => Ok(HookOutcome::Block {
            reason: format!("manipulation phrase: `{phrase}`"),
            do_not_reply_again: false,
        }),
        ManipulationVerdict::Clear => Ok(HookOutcome::Continue),
    }
}
```

**Tunables:** add domain-specific phrases via
`with_extra_phrases(...)`. Replace the entire list via
`with_phrases(...)` — useful if you only care about a small
subset.

## OptOutMatcher — unsubscribe

**When to wire it:**

- Channel is regulated (CAN-SPAM, GDPR, WhatsApp Business).
- User can text a keyword to stop replies.

**Wire shape:**

```rust
let opt_out = OptOutMatcher::default();

async fn before_message(args: Value, ctx: HookCtx) -> Result<HookOutcome, ToolError> {
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    match opt_out.evaluate(body) {
        OptOutVerdict::OptOut { keyword } => {
            // Persist the opt-out via ConsentTracker so future
            // outbound is also gated.
            CONSENT.lock().unwrap().opt_out(
                ctx.binding().map(|b| b.account_id.as_deref().unwrap_or("default")).unwrap_or("default"),
                "stop_keyword",
            );
            Ok(HookOutcome::Block {
                reason: format!("opt-out keyword: `{keyword}`"),
                do_not_reply_again: true,
            })
        }
        OptOutVerdict::Clear => Ok(HookOutcome::Continue),
    }
}
```

Whole-word matching avoids substring false positives — `"baja"`
in `"darse de baja"` matches; `"baja"` inside `"bajaron"` does
not.

## PiiRedactor — strip PII before LLM

**When to wire it:**

- Compliance rules say card / phone / email cannot leave the
  trust boundary.
- LLM provider's terms of service constrain what you can send.

**Wire shape:**

```rust
let redactor = PiiRedactor::new().with_luhn(true);

async fn before_message(args: Value, _ctx: HookCtx) -> Result<HookOutcome, ToolError> {
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let (clean, stats) = redactor.redact(body);
    if stats.total() == 0 {
        return Ok(HookOutcome::Continue);
    }
    Ok(HookOutcome::Transform {
        transformed_body: clean,
        reason: Some(format!(
            "redacted: {} cards, {} phones, {} emails",
            stats.cards_redacted, stats.phones_redacted, stats.emails_redacted
        )),
        do_not_reply_again: false,
    })
}
```

**Tunables:** `with_luhn(true)` filters Luhn-invalid
16-digit runs out of the card path (cuts false positives).
`skip_phones() / skip_cards() / skip_emails()` turn off
individual categories.

## RateLimitPerUser — token bucket

**When to wire it:**

- One user can spam.
- Protect downstream services (LLM provider, your DB, your
  outbound channel API).

**Wire shape:**

```rust
let mut limiter = RateLimitPerUser::flat(20, Duration::from_secs(60));

async fn before_message(args: Value, ctx: HookCtx) -> Result<HookOutcome, ToolError> {
    let user_key = ctx
        .inbound()
        .map(|m| m.from.clone())
        .unwrap_or_else(|| "anon".into());
    match limiter.try_acquire(&user_key) {
        RateLimitVerdict::Allowed { .. } => Ok(HookOutcome::Continue),
        RateLimitVerdict::Denied { retry_after } => Ok(HookOutcome::Block {
            reason: format!("rate-limited; retry in {retry_after:?}"),
            do_not_reply_again: false,
        }),
    }
}
```

**Tunables:** `RateLimitPerUser::new(rate, window, max)` lets
you specify a different burst max from the long-run rate. The
constructor clamps `max >= rate` so the long-run rate is
honoured.

## ConsentTracker — opt-in gate for outbound

**When to wire it:**

- Cold outbound (you message users who didn't message first).
- Regulatory: GDPR, CAN-SPAM, WhatsApp Business policy.

**Wire shape:**

```rust
let tracker = ConsentTracker::new();

// On every outbound dispatch:
fn can_send(user_key: &str) -> bool {
    TRACKER.lock().unwrap().allows_outbound(user_key)
}

// On opt-in form submission:
TRACKER.lock().unwrap().opt_in(user_key, "web_form");

// On opt-out keyword:
TRACKER.lock().unwrap().opt_out(user_key, "stop_keyword");
```

Default `Unknown` status means **no outbound** — CAN-SPAM
"express consent" default-deny. The audit log
(`history_for_user(...)`) gives you a per-user timestamped
record of every consent change.

## Composition

In production you wire ALL of them in one before-message hook:

```rust
async fn before_message(args: Value, ctx: HookCtx) -> Result<HookOutcome, ToolError> {
    let body = extract_body(&args);
    let user_key = extract_user_key(&ctx);

    // Order matters: cheapest checks first.
    if matches!(opt_out.evaluate(body), OptOutVerdict::OptOut { .. }) { return block_with_do_not_reply(); }
    if matches!(manipulation.evaluate(body), ManipulationVerdict::Matched { .. }) { return block(); }
    if let RateLimitVerdict::Denied { .. } = limiter.try_acquire(user_key) { return block_rate_limited(); }
    if let LoopVerdict::Repetition { .. } | LoopVerdict::AutoReplySignature { .. } = loop_detector.record_and_evaluate(body) {
        return block_loop();
    }

    // PII redaction LAST so the redacted body goes to the agent.
    let (clean, stats) = pii.redact(body);
    if stats.total() > 0 {
        return Ok(HookOutcome::Transform { transformed_body: clean, .. });
    }

    Ok(HookOutcome::Continue)
}
```

## See also

- [contract.md](./contract.md) — wire protocol spec.
- [rust.md](./rust.md) — Rust SDK reference.
- Phase 83.3 hook interceptor (vote-to-block) — the interceptor
  that consumes these verdicts.
- Phase 82.1 BindingContext — supplies `agent_id`, `channel`,
  `account_id` keys for per-tenant compliance state.
