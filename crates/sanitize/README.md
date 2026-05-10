# nexo-sanitize

Tiny CSS / URL input sanitisation helpers shared by every nexo
microapp that renders operator-authored content.

## Surface

```rust
use nexo_sanitize::{sanitize_color, sanitize_url};

// CSS colour — accepts #rgb / #rrggbb / #rrggbbaa hex + a small
// named palette. Anything else falls back to `default`.
let safe = sanitize_color("#ff0000", "#000000");          // "#ff0000"
let safe = sanitize_color("expression(alert(1))", "#000"); // "#000"

// http/https URL — rejects javascript:, data:, file:, scheme-
// relative `//`, and any character that could break out of a
// CSS `url(...)` wrapper or an HTML `background="..."` attr.
assert!(sanitize_url("https://cdn.example/bg.jpg").is_some());
assert!(sanitize_url("javascript:alert(1)").is_none());
```

## Why

These two helpers landed inside the marketing extension's email
renderer (where operators set per-row / per-column / per-page
backgrounds). They have nothing email-specific in them, and any
microapp that renders operator-set CSS values needs the same
guardrails — split out so the next microapp doesn't reinvent the
allowlist.
