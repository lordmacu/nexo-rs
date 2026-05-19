# Poller · Google Calendar

Google Calendar v3 poller — incremental sync of new + updated events,
dispatches each one to an agent as a prompt. Lead-time templates so
the agent can be reminded *N minutes before* an event. Subprocess
plugin under the Phase 96 poller v2 contract.

Source: standalone repo at
[`nexo-rs-poller-google-calendar`](https://github.com/lordmacu/nexo-rs-poller-google-calendar).

> **Credentials.** Reuses the OAuth client owned by the
> [`google` plugin](./google.md) — install that first and complete
> the one-time OAuth flow per agent before enabling this poller.

## Install

```bash
nexo plugin install lordmacu/nexo-rs-poller-google-calendar
# or
cargo install nexo-poller-google-calendar
```

## Configure

```yaml
- id: cal-reminders
  kind: google_calendar
  schedule: "*/10 * * * *"
  target_agent_id: assistant
  config:
    calendar_id: "primary"
    lead_time_minutes: 15           # fire 15 min before event start
    attendee_filter:
      include_domains: ["client.com"]
    template: |
      Upcoming: {{summary}}
      Start: {{start}}
      Attendees: {{attendees}}
      Location: {{location}}
```

Cursor-driven dedupe (Google Calendar's `syncToken`) ensures the
poller only sees deltas after the first full sync.

## See also

- [Google plugin](./google.md) — OAuth setup
- [Poller v2 architecture](../architecture/poller-v2.md)
- [Build a poller plugin (V2)](../recipes/poller-plugin.md)
