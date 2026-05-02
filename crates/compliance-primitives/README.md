# nexo-compliance-primitives

Reusable conversational-compliance primitives for [Nexo](https://github.com/lordmacu/nexo-rs)
microapps. Six small structs every conversational microapp needs:

| Primitive | Decision |
|---|---|
| `AntiLoopDetector` | Block on repetition / auto-reply signature |
| `AntiManipulationMatcher` | Block on prompt-injection / role-hijack |
| `OptOutMatcher` | Block + anti-loop on opt-out keyword |
| `PiiRedactor` | Transform: strip PII before LLM sees text |
| `RateLimitPerUser` | Block on token-bucket exhausted |
| `ConsentTracker` | Default-deny outbound until explicit opt-in |

Pure data + arithmetic — no async runtime, no persistence, no
LLM dep. Plug each primitive into the Phase 83.3 hook
interceptor and configure thresholds via `extensions_config`.

## Quick start

```rust
use nexo_compliance_primitives::{AntiLoopDetector, LoopVerdict};
use std::time::Duration;

let mut detector = AntiLoopDetector::new(3, Duration::from_secs(300));

match detector.record_and_evaluate("hola") {
    LoopVerdict::Repetition { count } => { /* vote Block */ }
    LoopVerdict::AutoReplySignature { phrase } => { /* vote Block + anti-loop */ }
    LoopVerdict::Clear => { /* proceed */ }
}
```

See [docs/src/microapps/compliance-primitives.md](https://github.com/lordmacu/nexo-rs/blob/main/docs/src/microapps/compliance-primitives.md)
for symptom-to-primitive lookup + per-primitive wire shapes
+ end-to-end composition.

## License

Dual MIT / Apache-2.0.
