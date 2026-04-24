# 1Password Extension (Rust)

Stdio extension that reads 1Password secrets through the `op` CLI using a
**service-account token** — non-interactive, headless, suitable for a
Linux daemon. Skips OpenClaw's tmux+desktop-app dance that only applies
to macOS.

## Tools

- `status` — op binary + token presence + reveal policy + limits
- `whoami` — `op whoami --format json` (validates the token)
- `list_vaults` — id + name only (never includes fields)
- `list_items` — `{id, title, category, vault, tags, updated_at}` per item;
  any field-value data from `op item list` is stripped before return
- `read_secret` — `op read op://Vault/Item/field`; returns metadata only
  unless `OP_ALLOW_REVEAL=true`

## Requires

- `op` binary on `PATH` (declared as `requires.bins` in `plugin.toml`)
- `OP_SERVICE_ACCOUNT_TOKEN` env var with a valid service-account token
  (`ops_...`). Declared as `requires.env`.

## Reveal policy (IMPORTANT)

`read_secret` is **opt-in reveal** by default:

- No `OP_ALLOW_REVEAL` → returns `{reveal: false, length, fingerprint_sha256_prefix}`.
  The LLM can verify *which* secret was read (via fingerprint) but cannot
  see the value.
- `OP_ALLOW_REVEAL=true|1|yes` → returns additionally `{value: "...", reveal: true}`.
  Secret flows through the LLM context and therefore through transcripts,
  memory, and any downstream destination the agent chooses.

Default is conservative: an agent turning on reveal is a deliberate
operator choice per deployment.

Always prefer **not to reveal**: the agent asks 1Password to *perform*
something with the secret (via a sibling extension that injects the value
where needed — future work). Reveal is for truly unavoidable cases.

## Error codes

| Code | Meaning |
|------|---------|
| -32040 | `op` binary not found (and OP_BIN_OVERRIDE unset) |
| -32041 | OP_SERVICE_ACCOUNT_TOKEN not set |
| -32031 | subprocess spawn failed |
| -32042 | op exited non-zero (stderr preview in message) |
| -32033 | subprocess timeout |
| -32602 | bad `reference` (wildcards, wrong scheme, missing segments) |
| -32006 | op returned non-JSON where JSON expected |
| -32043 | reveal denied (env flag off) — informational |
| -32034 | io / filesystem error |

## Environment

- `OP_SERVICE_ACCOUNT_TOKEN` — required for every non-status call
- `OP_ALLOW_REVEAL` — default off; set `true`/`1`/`yes` to include values
- `OP_TIMEOUT_SECS` — default 30s, max 300s
- `OP_BIN_OVERRIDE` — absolute path to a fake `op` for tests

## Build & test

```bash
cargo build --release --manifest-path extensions/onepassword/Cargo.toml
cargo test           --manifest-path extensions/onepassword/Cargo.toml
```

16 tests: 5 unit (reference validation, reveal_allowed env, binary
resolution) + 11 integration (status, whoami, list_vaults strips
unexpected fields, list_items strips `fields[*].value`, read metadata-only,
read reveal, wildcard/non-op rejected, missing token, non-zero exit,
unknown tool). Integration writes a bash fake `op` per test via
`OP_BIN_OVERRIDE`.

## Smoke

```bash
OP_SERVICE_ACCOUNT_TOKEN=ops_realtoken \
OP_ALLOW_REVEAL=true \
  echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_secret","arguments":{"reference":"op://Prod/Stripe/api_key"}}}' \
  | ./target/release/onepassword
```
