# template-poller-python

A 130-LoC Python extension that ships one poller `kind`
(`template_poll`). Use it as the starting point for any third-party
poller — Slack, Linear, Notion, your internal CRM, anything that
exposes a JSON HTTP endpoint and benefits from the framework's
scheduling + state + Phase 17 credentials + audit log.

## Install

```bash
agent ext install ./extensions/template-poller-python --enable --link
```

`--link` symlinks the directory so edits to `main.py` are picked up
on next daemon restart without re-running install.

## Declare a job

```yaml
# config/pollers.yaml
pollers:
  jobs:
    - id: ana_template_demo
      kind: template_poll          # matches plugin.toml capabilities.pollers
      agent: ana
      schedule: { every_secs: 30 }
      config:
        deliver:
          channel: telegram
          to: "1194292426"
```

## Verify

```bash
agent pollers list                # ana_template_demo shows up
agent pollers run ana_template_demo
```

You should see a Telegram message every 30 s saying
`template_poll tick #N`.

## Wire protocol

The extension speaks one JSON-RPC method per line on stdin/stdout:

| method        | params                                                 | result                                                                      |
|---------------|---------------------------------------------------------|------------------------------------------------------------------------------|
| `initialize`  | `{ name, version }`                                     | `{ name, version, capabilities: { pollers: [...] } }`                        |
| `poll_tick`   | `{ kind, job_id, agent_id, cursor, config, now }`       | `{ items_seen, items_dispatched, deliver: [...], next_cursor, next_interval_secs }` |
| `shutdown`    | `{}`                                                    | `{}`                                                                          |

`cursor` is base64-url (no padding). The extension persists its
own opaque bytes — Gmail's `historyId`, RSS's `etag`, Calendar's
`syncToken`, your service's pagination token. The runtime
round-trips it: whatever you put in `next_cursor` lands in
`params.cursor` next tick.

## Error semantics

Use JSON-RPC error codes to tell the runner how to react:

| code     | meaning   | runtime reaction                              |
|----------|-----------|-----------------------------------------------|
| `-32001` | Transient | exponential backoff, retry                    |
| `-32002` | Permanent | auto-pause job + dispatch failure-to alert    |
| `-32602` | Config    | kill this job at boot; siblings keep going    |
| other    | Transient | default-deny: retry                            |

## What you don't have to write

- Schedule (`every`, `cron`, `at`) + jitter
- SQLite cursor + per-(channel, instance) lease
- Exponential backoff + per-job circuit breaker
- Outbound dispatch to `plugin.outbound.<ch>.<instance>` resolved
  via Phase 17 credentials
- 7 Prometheus series + audit log under `target=credentials.audit`
- Admin HTTP at `127.0.0.1:9091/admin/pollers/*` + CLI
  `agent pollers …`
- 6 generic `pollers_*` LLM tools registered per agent

The runner gives all of that to your extension for free.

## See also

- [Generic poller subsystem (config/pollers.md)](../../docs/src/config/pollers.md)
- [Build a poller module (in-tree Rust)](../../docs/src/recipes/build-a-poller.md)
