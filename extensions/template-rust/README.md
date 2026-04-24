# Rust Template Extension

Standalone Rust extension for agent-rs. Single binary, stdin/stdout JSON-RPC.

This crate is deliberately **outside** the agent workspace so it has its own
`Cargo.lock` and doesn't inherit the agent's dependency graph.

## Protocol

Line-delimited JSON-RPC 2.0. See `src/main.rs` and `src/protocol.rs`.

## Sample tools

- `ping` — zero-arg smoke test
- `add` — sum of `a` and `b`

## Copy & customize

```bash
cp -r extensions/template-rust extensions/my-tool
cd extensions/my-tool
```

Edit `Cargo.toml`:

```toml
[package]
name = "my-tool"                    # must match the ID you set in plugin.toml
```

Edit `plugin.toml`:

```toml
[plugin]
id = "my-tool"                      # must match ^[a-z][a-z0-9_-]*$

[capabilities]
# Tool names must match ^[a-z][a-z0-9_]*$ — snake_case only, no
# hyphens. Rejected at manifest parse: `get-weather`. OK: `get_weather`.
tools = ["get_weather"]

[transport]
kind = "stdio"
command = "./my-tool"               # = the built binary name
```

Swap the sample tools in `src/tools.rs`.

## Build & install

```bash
cd extensions/my-tool
cargo build --release
cp target/release/my-tool ./my-tool
```

Restart the agent. Expected logs:

```
discovered extension id=my-tool transport=stdio
extension runtime ready ext=my-tool tools=N
extension tool registered agent=<your-agent> ext=my-tool tool=ext_my-tool_<name>
```

## Notes

- Binary must sit at the path `plugin.toml` declares (relative to the
  extension directory). The agent spawns the binary with `cwd` set to the
  extension directory.
- `shutdown` is best-effort: the host may tear down without waiting for your
  ACK (notably on NATS transports/timeouts). Do not gate critical cleanup only
  behind a successful shutdown reply.
- Combined tool name `ext_<id>_<name>` must stay ≤64 chars (OpenAI/MiniMax
  limit).
- The shipping `config/extensions.yaml` has `disabled: ["template-rust"]`
  so a fresh clone doesn't log a spawn error at boot. Remove the entry
  (or run `agent ext enable template-rust`) after you build and install
  the binary.
- Until you build the binary the agent logs `extension spawn failed` at
  startup — that is the error-path working correctly, not a bug.
- Write to stderr for logs; the agent forwards stderr lines to its own
  tracing output.
