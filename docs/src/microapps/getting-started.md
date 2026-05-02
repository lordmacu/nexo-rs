# Getting started: build a microapp in 1 hour

This walks the **first hour** of building a nexo microapp end to
end. Goal: by the end of this page you have a working
hello-world microapp running against a local nexo daemon, with
one tool the LLM can call.

For the language-agnostic protocol spec, see
[contract.md](./contract.md). For the full Rust SDK reference,
see [rust.md](./rust.md).

## Prerequisites

```text
✅ Rust 1.75+ (`rustup default stable`)
✅ A working nexo-rs checkout (this repo)
✅ A configured nexo daemon (one agent, one channel binding)
```

You don't need crates.io publish keys, npm, or a CI pipeline.
Local files only.

## Step 1 — copy the template (5 min)

```bash
# From your work directory (NOT inside nexo-rs):
cp -r /path/to/nexo-rs/extensions/template-microapp-rust ./mi-microapp
cd ./mi-microapp

# Rename inside Cargo.toml + plugin.toml + src/main.rs:
sed -i 's/template-microapp-rust/mi-microapp/g' Cargo.toml plugin.toml src/main.rs

git init
git add -A
git commit -m "scaffold from nexo template"
```

Now you have:

```text
mi-microapp/
├── Cargo.toml          # depends on nexo-microapp-sdk
├── plugin.toml         # capabilities + transport declaration
├── README.md           # rename checklist + porting guide
└── src/main.rs         # ~100 LOC including comments
```

## Step 2 — write your first tool (15 min)

Open `src/main.rs`. Replace the `greet_tool` body with your
domain logic:

```rust
async fn buscar_cliente(args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    let phone = args
        .get("phone")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::wire("phone required"))?;

    // BindingContext threads the agent + channel + account
    // (Phase 82.1) through every call.
    let agent = ctx.binding().map(|b| b.agent_id.clone()).unwrap_or_default();

    Ok(ToolReply::ok_json(json!({
        "agent": agent,
        "phone": phone,
        "found": false,
        "lead_id": null,
    })))
}
```

Register it in `main()`:

```rust
let app = Microapp::new("mi-microapp", env!("CARGO_PKG_VERSION"))
    .with_tool("mi_microapp_buscar_cliente", buscar_cliente);
```

Build:

```bash
cargo build --release
```

The binary lands in `./target/release/mi-microapp`.

## Step 3 — smoke test the wire (5 min)

The microapp speaks line-delimited JSON-RPC over stdio. You can
exercise it without the daemon:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  | ./target/release/mi-microapp
```

Expected output (one line, JSON):

```json
{"jsonrpc":"2.0","id":1,"result":{
  "tools":["mi_microapp_buscar_cliente"],
  "hooks":["before_message"],
  "server_info":{"name":"mi-microapp","version":"0.1.0"}
}}
```

`tools/call` works the same way:

```bash
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mi_microapp_buscar_cliente","arguments":{"phone":"+57311"}}}' \
  | ./target/release/mi-microapp
```

If both calls return clean JSON, your microapp speaks the
contract.

## Step 4 — install into the daemon (15 min)

Copy the build artifact + `plugin.toml` into the daemon's
`extensions/` directory:

```bash
mkdir -p ~/.nexo/extensions/mi-microapp
cp target/release/mi-microapp ~/.nexo/extensions/mi-microapp/
cp plugin.toml ~/.nexo/extensions/mi-microapp/
```

Reference the microapp from `~/.nexo/config/extensions.yaml`:

```yaml
extensions:
  entries:
    mi-microapp:
      enabled: true
      capabilities_grant:
        - dispatch_outbound       # if your tools call nexo/dispatch
        # add more as your microapp needs them
```

Reference its tool from `~/.nexo/config/agents.yaml`:

```yaml
agents:
  - id: ana
    extensions: [mi-microapp]
    allowed_tools:
      - mi_microapp_buscar_cliente   # appears in the LLM tool catalogue
```

Restart the daemon:

```bash
nexo daemon restart
# or for dev: kill the process and re-run `nexo daemon start`
```

## Step 5 — verify the LLM sees your tool (10 min)

Send a test message through your bound channel. The LLM should
see `mi_microapp_buscar_cliente` in its tool catalogue and call
it on relevant prompts.

Check the daemon logs:

```bash
nexo logs --tail | grep mi-microapp
```

You should see:
- `extensions: spawned mi-microapp pid=...`
- `extensions: mi-microapp -> initialize ok`
- `tools/call mi_microapp_buscar_cliente {"phone": "..."}`

If the tool is being called but the LLM doesn't surface it
correctly, the prompt may not have descriptions rich enough —
add a `description` to your tool registration.

## Step 6 — add per-agent config (10 min)

Different agents may need different microapp behaviour. Use
[Phase 83.1](../../PHASES-microapps.md) `extensions_config`:

```yaml
agents:
  - id: ana
    extensions: [mi-microapp]
    extensions_config:
      mi-microapp:
        regional: bogota
        api_token_env: ANA_ETB_TOKEN

  - id: maria
    extensions: [mi-microapp]
    extensions_config:
      mi-microapp:
        regional: cali
        api_token_env: MARIA_ETB_TOKEN
```

In your handler, the `BindingContext.agent_id` lets you key
into a per-agent config map you build at `initialize` time.
Until 83.1.b ships the JSON-RPC propagation, the operator can
also pass the config via env vars and your microapp reads them
on boot.

## Common patterns

### Multi-tenant SaaS

You're shipping a single microapp binary that serves multiple
tenants. See [extensions/multi-tenant-saas.md](../extensions/multi-tenant-saas.md).
Key idea: every tool call carries `BindingContext.account_id`
(Phase 82.1) — key your per-tenant SQLite tables on it.

### Compliance enforcement

Drop in [`nexo-compliance-primitives`](./contract.md#worked-example-rust-sdk-shortcut)
to anti-loop / anti-manipulation / opt-out / PII-redact / rate
limit / consent track. Wire each primitive into a Phase 83.3
hook that votes `Block` or `Transform` before the LLM sees
the inbound.

### Outbound dispatch

Need your microapp to send a WhatsApp / Telegram / email reply?
Use the `nexo-microapp-sdk` `outbound` feature:

```toml
[dependencies]
nexo-microapp-sdk = { path = "...", features = ["outbound"] }
```

Then `ctx.outbound().dispatch(...)` from inside any tool
handler. See [extensions/stdio.md](../extensions/stdio.md).

## Troubleshooting

| Symptom | Fix |
|---|---|
| `extensions: mi-microapp -> initialize timed out` | Microapp didn't reply within 30 s. Check stderr; missing tokio runtime is the most common cause. |
| `tool 'mi_microapp_x' not in catalogue` | Tool name missing the `<extension_id>_` prefix. Daemon enforces the namespacing. |
| `capability denied: dispatch_outbound` | Operator forgot to add the capability to `extensions.yaml.entries.<id>.capabilities_grant`. |
| `404 unknown method: hooks/before_message` | The hook name in your `with_hook(...)` call doesn't match a daemon-emitted hook. Check `crates/extensions/src/runtime/mod.rs::HOOK_NAMES`. |
| Build fails: `nexo-microapp-sdk = "0.1"` not found | SDK isn't on crates.io yet (Phase 83.14). Use `path = "..."` against your nexo-rs checkout. |

## Next steps

You have a working microapp. Now:

- Read [contract.md](./contract.md) end-to-end — the wire spec
  is short, and every detail matters for compat.
- Read [rust.md](./rust.md) for the full SDK reference.
- For multi-tenant SaaS: [extensions/multi-tenant-saas.md](../extensions/multi-tenant-saas.md).
- For compliance gating: pull in `nexo-compliance-primitives`
  and wire its primitives into your Phase 83.3 hooks.
