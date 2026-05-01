# template-microapp-rust

Skeleton stdio microapp for nexo. Copy + rename to bootstrap a
new domain-specific microapp using the `nexo-microapp-sdk`
builder API.

## What this template demonstrates

- Two example tool handlers (`greet`, `ping`) registered via
  `Microapp::with_tool`.
- `BindingContext` access from a tool handler — the agent +
  channel + account triple the daemon threads through every
  call (Phase 82.1).
- One observer hook (`before_message`) registered via
  `with_hook` that always votes `Continue`.
- Idiomatic SDK error handling via `ToolError` / `ToolReply`.

## Quick start

```bash
# Build
cargo build -p template-microapp-rust --release

# Smoke test the wire protocol — the SDK runs a minimal
# JSON-RPC handshake.
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  | ./target/release/template-microapp-rust
```

The expected `initialize` reply lists `greet` + `ping` in the
microapp's tool catalogue plus the version string from
`Cargo.toml`.

## Wiring into nexo

1. Build the binary as above.
2. Drop the directory under your nexo install's `extensions/`:
   ```
   extensions/
     my-microapp/                    # renamed from template
       my-microapp                   # the binary
       plugin.toml
   ```
3. Edit `plugin.toml` — change `id`, `name`, and the tool list
   under `[capabilities]`.
4. Reference the microapp from `config/extensions.yaml`:
   ```yaml
   extensions:
     entries:
       my-microapp:
         enabled: true
         capabilities_grant:
           - dispatch_outbound       # if your tools call nexo/dispatch
           - transcripts_subscribe   # if you listen to transcript_appended
   ```
5. Restart the daemon. The LLM will see your tools in its
   catalogue on the next turn.

## Renaming the template

When you copy the directory:

1. Rename `extensions/template-microapp-rust/` → your name.
2. Update `Cargo.toml`:
   - `name`, `description`, `[[bin]] name` + `path` if you keep
     `src/main.rs`.
   - Switch `nexo-microapp-sdk` from `path = "..."` to
     `version = "0.1"` once the SDK ships to crates.io. Until
     then keep the in-tree path dep.
3. Update `plugin.toml`:
   - `id`, `name`, `description`.
   - The `[capabilities] tools = [...]` list.
   - The `[transport] command = "./<binary-name>"` line.
4. Update tool names in `src/main.rs` to match the
   `<extension_id>_<tool>` namespacing rule from
   `docs/src/microapps/contract.md` (the daemon validates this).
5. Remove this README's contents and write your own.

## Porting to other languages

The microapp **contract** is the line-delimited JSON-RPC stdio
loop documented in
[`docs/src/microapps/contract.md`](../../docs/src/microapps/contract.md).
This template is the Rust convenience wrapper. To target Python,
TypeScript, Go, or any other runtime:

1. **Read the contract doc end-to-end.** It is the canonical
   spec; everything below is just a cheatsheet.
2. **Implement the JSON-RPC loop directly** using your
   language's stdlib. The contract doc has worked examples for
   Python, Go, and TypeScript.
3. **Match the conventions:**
   - Tool name namespacing: `<extension_id>_<tool>`.
   - Reserved error code range: -32000 to -32099.
   - `app:<uuid>` id prefix on outbound (microapp → daemon)
     calls.
   - Flush stdout after every JSON-RPC frame.
4. **Update `plugin.toml`** to point at your interpreter:
   ```toml
   [transport]
   kind = "stdio"
   command = "python3"
   args    = ["./main.py"]
   ```
   or
   ```toml
   [transport]
   kind = "stdio"
   command = "node"
   args    = ["./dist/main.js"]
   ```

The Rust SDK is the recommended path because it stays in
lockstep with the daemon's contract version (additive fields,
deprecation cycles). Hand-rolled implementations in other
languages MUST follow the contract doc's compat rules.

## Where to go next

| Need | Where to look |
|---|---|
| Full contract spec | [`docs/src/microapps/contract.md`](../../docs/src/microapps/contract.md) |
| Rust SDK reference | [`docs/src/microapps/rust.md`](../../docs/src/microapps/rust.md) |
| Operator surface (admin RPC) | [`docs/src/microapps/admin-rpc.md`](../../docs/src/microapps/admin-rpc.md) |
| Multi-tenant SaaS pattern | [`docs/src/extensions/multi-tenant-saas.md`](../../docs/src/extensions/multi-tenant-saas.md) |
| Per-extension state dir | [`docs/src/extensions/state-management.md`](../../docs/src/extensions/state-management.md) |
| Outbound dispatch (Phase 82.3) | [`docs/src/extensions/stdio.md`](../../docs/src/extensions/stdio.md) |
