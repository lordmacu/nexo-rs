# Plugin contract (out-of-tree subprocess plugins)

The authoritative wire-format spec for nexo plugins lives at the
workspace root: [`nexo-plugin-contract.md`](https://github.com/lordmacu/nexo-rs/blob/main/nexo-plugin-contract.md).

It documents:

- Transport (newline-delimited JSON-RPC 2.0 over stdin/stdout)
- Manifest `[plugin.entrypoint]` section
- Lifecycle (`initialize`, `shutdown`)
- Broker bridge (`broker.event`, `broker.publish`)
- Topic allowlist (derived from `[[plugin.channels.register]]`)
- Error codes
- Backpressure semantics
- Examples in Rust (Phase 81.15.a, shipped) plus skeletons for
  Python (Phase 31.4, planned) and TypeScript (Phase 31.5,
  planned)
- Versioning + compatibility policy

Read the contract before authoring a plugin in any language —
that file is the single source of truth.

## Reference implementations in this workspace

- **Host adapter**: `crates/core/src/agent/nexo_plugin_registry/subprocess.rs`
  (`SubprocessNexoPlugin`).
- **Rust child SDK**: `crates/microapp-sdk/src/plugin.rs`
  (`PluginAdapter`, gated behind the `plugin` Cargo feature).
