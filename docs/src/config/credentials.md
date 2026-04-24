# Per-agent credentials

Bind each agent to specific WhatsApp / Telegram / Google accounts so
outbound traffic originates from the right number, bot, or mailbox —
never from a shared pool.

## Mental model

Three layers:

1. **Plugin instance** — a labelled WhatsApp session or Telegram bot in
   `config/plugins/{whatsapp,telegram}.yaml`. Each instance owns its
   own token / session_dir and an optional `allow_agents` list.
2. **Google account** — an entry in the optional
   `config/plugins/google-auth.yaml`. Each account is 1:1 with an
   `agent_id`.
3. **Agent binding** — in `config/agents.d/<agent>.yaml`, the
   `credentials:` block pins the agent to the instance / account it
   may use for outbound tool calls.

The runtime runs a boot-time **gauntlet** that cross-checks all three
layers before any plugin boots. Every invariant violation surfaces in
a single report so you can fix the full YAML in one edit.

## Config schemas

### `config/agents.d/ana.yaml`

```yaml
agents:
  - id: ana
    credentials:
      whatsapp: personal        # must match whatsapp.yaml instance
      telegram: ana_bot         # must match telegram.yaml instance
      google:   ana@gmail.com   # must match google-auth.yaml accounts[].id
      # Opt-out for the symmetric-binding warning when inbound bot and
      # outbound bot are intentionally different:
      # telegram_asymmetric: true
    inbound_bindings:
      - { plugin: whatsapp, instance: personal }
      - { plugin: telegram, instance: ana_bot }
```

### `config/plugins/whatsapp.yaml`

```yaml
whatsapp:
  - instance: personal
    session_dir: ./data/workspace/ana/whatsapp/personal
    media_dir:   ./data/media/whatsapp/personal
    allow_agents: [ana]           # defense-in-depth ACL
  - instance: work
    session_dir: ./data/workspace/kate/whatsapp/work
    media_dir:   ./data/media/whatsapp/work
    allow_agents: [kate]
```

### `config/plugins/telegram.yaml`

```yaml
telegram:
  - instance: ana_bot
    token: ${file:./secrets/telegram/ana_token.txt}
    allow_agents: [ana]
    allowlist:
      chat_ids: [1194292426]
  - instance: kate_bot
    token: ${file:./secrets/telegram/kate_token.txt}
    allow_agents: [kate]
```

### `config/plugins/google-auth.yaml`

```yaml
google_auth:
  accounts:
    - id: ana@gmail.com
      agent_id: ana                       # 1:1 — the gauntlet enforces it
      client_id_path:     ./secrets/google/ana_client_id.txt
      client_secret_path: ./secrets/google/ana_client_secret.txt
      token_path:         ./secrets/google/ana_token.json
      scopes:
        - https://www.googleapis.com/auth/gmail.modify
```

Agents that still declare the legacy inline `google_auth` block are
auto-migrated into this store on boot (a warning tells you to migrate).

## What the gauntlet validates

| Check                                                    | Lenient | Strict |
|----------------------------------------------------------|---------|--------|
| Duplicate `session_dir` across instances                 | error   | error  |
| `session_dir` that is a parent of another                | error   | error  |
| Credential file with lax permissions (linux 0o077)       | error   | error  |
| `credentials.<ch>` points to an instance that does not exist | error | error |
| Agent listens on >1 instance without declaring `credentials.<ch>` | error | error |
| Instance `allow_agents` excludes a binding agent         | error   | error  |
| Inbound instance ≠ outbound instance (no `<ch>_asymmetric`) | warn  | error  |
| Inline `agents.<id>.google_auth` without matching `google-auth.yaml` | warn | warn |

Linux permission check is skipped for `/run/secrets/*` (Docker secrets)
and can be disabled entirely with `CHAT_AUTH_SKIP_PERM_CHECK=1`.

## Topics

Outbound tool calls land on instance-suffixed topics when the resolver
has a binding:

```
plugin.outbound.whatsapp.<instance>
plugin.outbound.telegram.<instance>
```

Unlabelled (`instance: None`) plugin entries keep publishing to the
legacy bare topic `plugin.outbound.whatsapp` / `plugin.outbound.telegram`
for full back-compat.

## CLI gate

```bash
# Run the full gauntlet without booting the daemon. Exits 0 clean,
# 1 on errors, 2 on warnings-only.
agent --config ./config --check-config

# Promote warnings to errors (CI lane).
agent --config ./config --check-config --strict
```

The gate scans `agents.yaml`, every `agents.d/*.yaml`,
`whatsapp.yaml`, `telegram.yaml`, and `google-auth.yaml`. Sample
failure:

```
credentials: FAILED with 1 error(s):
   1. agent 'ana_per_binding_example' binds credentials.telegram='ana_tg' but no such telegram instance exists (available: [])
```

## Secrets in logs

The credential layer never logs a raw account id. Every reference is
via an 8-byte `sha256(account_id)` **fingerprint** rendered as hex:

```
2025-04-24T16:03:42Z INFO credentials.audit agent="ana" channel="whatsapp" fp=a3f2…7c direction=outbound
```

The fingerprint is pinned — switching the algorithm is an explicit
breaking change tracked by `crates/auth/tests/fingerprint_stability.rs`.

## Observability

Nine Prometheus series land at `/metrics`:

| Series                                            | Type      | Labels                                         |
|---------------------------------------------------|-----------|-------------------------------------------------|
| `credentials_accounts_total`                      | gauge     | `channel`                                       |
| `credentials_bindings_total`                      | gauge     | `agent`, `channel`                              |
| `channel_account_usage_total`                     | counter   | `agent`, `channel`, `direction`, `instance`     |
| `channel_acl_denied_total`                        | counter   | `agent`, `channel`, `instance`                  |
| `credentials_resolve_errors_total`                | counter   | `channel`, `reason`                             |
| `credentials_breaker_state`                       | gauge     | `channel`, `instance`                           |
| `credentials_boot_validation_errors_total`        | counter   | `kind`                                          |
| `credentials_insecure_paths_total`                | gauge     | —                                               |
| `credentials_google_token_refresh_total`          | counter   | `account_fp`, `outcome`                         |

## Back-compat

- Configs without a `credentials:` block keep working — the resolver
  infers outbound from the single `inbound_bindings` entry when it is
  unambiguous; otherwise outbound tools are marked unbound and fall
  back to the legacy bare topic.
- Plugin entries with `instance: None` stay on the legacy bare topic.
- `agents.<id>.google_auth` still registers `google_*` tools for that
  agent; `google-auth.yaml` is preferred going forward.

## Migrating

1. Add `instance:` + `allow_agents:` to each entry in
   `whatsapp.yaml` / `telegram.yaml`.
2. Create `config/plugins/google-auth.yaml` with one `accounts[]` per
   agent that needs Gmail.
3. Add `credentials:` to each `agents.d/*.yaml`.
4. Run `agent --check-config --strict`. Fix every listed error.
5. Commit.
