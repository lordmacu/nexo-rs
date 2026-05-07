# nexo-cdp

Chrome DevTools Protocol (CDP) WebSocket client for Nexo plugins.

Lifted out of `crates/plugins/browser/src/cdp/` as part of follow-up
`nexo-cdp-extract` (Phase 81.17.c) so any plugin or tool driving a
CDP-speaking endpoint can depend on it. The first consumer is
`nexo-plugin-browser` (out-of-tree subprocess + dormant in-tree
mirror at `crates/plugins/browser/`).

## Public API

```rust
use nexo_cdp::{CdpClient, CdpSession};
```

- `CdpClient` — connects to a `ws://host:port/devtools/browser/<guid>`
  endpoint, multiplexes request/response correlation via id, fans out
  `Network.*` / `Page.*` event notifications via broadcast channel.
- `CdpSession` — attaches to a `Target` (page / iframe), forwards
  per-target commands.

## License

MIT OR Apache-2.0 (same as the workspace).
