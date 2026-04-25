# 1Password extension

A bundled stdio extension that wraps the [`op` CLI](https://developer.1password.com/docs/cli/)
with a service-account token. Read-only: it never creates or edits
secrets. Two main use cases:

- Look up a secret you don't already have in env (`read_secret`).
- Use a secret in a command without ever exposing it to the agent
  (`inject_template`).

Source: `extensions/onepassword/`. Skill prompt: `skills/onepassword/SKILL.md`.

## Tools

| Tool | Reveals secret? | Audited |
|------|-----------------|---------|
| `status` | no | no |
| `whoami` | no | no |
| `list_vaults` | no | no |
| `list_items` | no — strips field values | no |
| `read_secret` | only if `OP_ALLOW_REVEAL=true` | yes |
| `inject_template` | template-only mode reveals only with `OP_ALLOW_REVEAL=true`; exec mode never reveals to the LLM | yes |

## `read_secret`

```json
{ "action": "read_secret", "reference": "op://Prod/Stripe/api_key" }
```

Default response (reveal off):

```json
{
  "ok": true,
  "reference": "op://Prod/Stripe/api_key",
  "vault": "Prod", "item": "Stripe", "field": "api_key",
  "length": 26,
  "fingerprint_sha256_prefix": "3f9a7c2e1b48d5a0",
  "reveal": false
}
```

With `OP_ALLOW_REVEAL=true|1|yes` set on the agent process, the
response also contains `{ "value": "...", "reveal": true }`.

## `inject_template`

Resolves `{{ op://Vault/Item/field }}` placeholders via `op inject`.
Two execution paths:

### Template-only

```json
{ "action": "inject_template",
  "template": "Authorization: Bearer {{ op://Prod/API/token }}\n" }
```

- Reveal off → `{ length, fingerprint_sha256_prefix, reveal: false }`
- Reveal on → `{ rendered: "Authorization: Bearer abc…", reveal: true }`

### Exec (piped to a command)

```json
{ "action": "inject_template",
  "template": "Bearer {{ op://Prod/API/token }}",
  "command": "curl",
  "args": ["-H", "@-", "https://api.example.com/me"] }
```

- `command` must be in `OP_INJECT_COMMAND_ALLOWLIST` (comma-separated).
  Default empty → exec mode disabled.
- Rendered template is **never returned** to the LLM. Only the
  downstream command's `exit_code`, `stdout` (capped at
  `max_stdout_bytes`, default 4096, max 16384), and `stderr`.
- Both `stdout` and `stderr` are redacted before being returned —
  Bearer JWT, `sk-…`, `sk-ant-…`, `AKIA…`, and 32+ char hex tokens
  are replaced with `[REDACTED:<label>]`.

### Dry run

```json
{ "action": "inject_template",
  "template": "{{ op://A/B/c }} {{ op://X/Y/z }}",
  "dry_run": true }
```

Validates each `op://` reference's shape without resolving values.
Returns `references_validated`.

## Configuration

Environment variables consumed by the extension:

| Var | Purpose | Default |
|-----|---------|---------|
| `OP_SERVICE_ACCOUNT_TOKEN` | required | — |
| `OP_ALLOW_REVEAL` | `true`/`1`/`yes` to allow value reveal | off |
| `OP_AUDIT_LOG_PATH` | JSONL audit log path | `./data/secrets-audit.jsonl` |
| `OP_INJECT_COMMAND_ALLOWLIST` | comma-separated allowed exec commands | empty (exec disabled) |
| `OP_INJECT_TIMEOUT_SECS` | per-call timeout (capped at `MAX_TIMEOUT_SECS`) | 30 |
| `OP_TIMEOUT_SECS` | per-call timeout for non-inject commands | 15 |
| `AGENT_ID` | injected by the host on spawn — appears in audit | — |
| `AGENT_SESSION_ID` | injected by the host on spawn | — |

## Audit log

`read_secret` and `inject_template` append one JSON line per call to
`OP_AUDIT_LOG_PATH`. The log is append-only and contains **only
metadata** — never the secret value.

```jsonl
{"ts":"2026-04-25T18:00:00Z","action":"read_secret","agent_id":"kate","session_id":"f1...","op_reference":"op://Prod/Stripe/token","fingerprint_sha256_prefix":"a1b2c3d4e5f6789a","reveal_allowed":false,"ok":true}
{"ts":"2026-04-25T18:00:05Z","action":"inject_template","agent_id":"kate","session_id":"f1...","references":["op://Prod/Stripe/token"],"command":"curl","args_count":4,"dry_run":false,"ok":true,"exit_code":0,"stdout_total_bytes":124,"stdout_returned_bytes":124,"stdout_truncated":false}
{"ts":"2026-04-25T18:00:10Z","action":"inject_template","agent_id":"kate","session_id":null,"references":["op://Bad/Ref"],"command":"rm","args_count":0,"dry_run":false,"ok":false,"error":"command_not_in_allowlist"}
```

Failures writing the log are reported to stderr and never block the
tool — the secret has already been read or piped; refusing to log
would be worst-of-both-worlds.

Rotate with `logrotate` or any append-aware rotator. Keeping the log
on a partition with limited write access (separate user, AppArmor,
or dedicated tmpfs) reduces forensic tampering surface.

## Threat model

- **The agent process is trusted.** Reveal is gated by an env var the
  operator controls; once on, the value is just a string in memory
  that flows through the LLM, transcripts, and any tool that touches
  it.
- **Exec mode is the recommended path** for any operation that does
  not require the agent to *see* the secret. The LLM only knows that
  the operation succeeded, not what the credential looked like.
- **Redaction is best-effort.** Stdout from a poorly-behaved command
  could still leak a secret in a shape we don't recognize. Cap the
  `max_stdout_bytes` aggressively when in doubt.
- **The audit log is not encrypted.** It contains references and
  fingerprints, not values. If even the references are sensitive,
  put the log on a permissioned filesystem.
