# Poller · Gmail

Gmail API poller — sweeps an inbox via OAuth, extracts addresses +
body via regex templates, dispatches matches to an agent. Subprocess
plugin; daemon spawns it per tick under the Phase 96 poller v2
contract.

Source: standalone repo at
[`nexo-rs-poller-gmail`](https://github.com/lordmacu/nexo-rs-poller-gmail).

> **Credentials.** Reuses the OAuth client owned by the
> [`google` plugin](./google.md) — install that first and complete
> the one-time OAuth flow per agent before enabling this poller.

## Install

```bash
nexo plugin install lordmacu/nexo-rs-poller-gmail
# or
cargo install nexo-poller-gmail
```

## Configure

```yaml
- id: gmail-leads
  kind: gmail
  schedule: "*/5 * * * *"
  target_agent_id: sales
  config:
    query: "is:unread label:inbox"     # standard Gmail query
    label_after_dispatch: "agent/seen" # mark to avoid replays
    body_extract:
      pattern: 'phone:\s*([+\d\s\-]+)'  # capture group → {{phone}}
      bind: phone
```

Each unseen message matching `query` dispatches as an agent prompt
with `from`, `subject`, `body`, plus any captured `body_extract`
fields. After dispatch, the configured label is applied so the next
tick skips it (cursor-driven dedupe).

## See also

- [Google plugin](./google.md) — OAuth setup
- [Poller v2 architecture](../architecture/poller-v2.md)
- [Build a poller plugin (V2)](../recipes/poller-plugin.md)
