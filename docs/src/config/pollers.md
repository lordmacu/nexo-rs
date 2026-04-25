# pollers.yaml

The Phase 19 generic poller subsystem. One runner orchestrates N
modules — each module is an `impl Poller` (gmail, rss, calendar,
webhook_poll, or anything you write yourself) — and every module
shares the same scheduler, lease, breaker, cursor persistence, and
outbound dispatch via Phase 17 credentials.

Source: `crates/poller/`, `crates/config/src/types/pollers.rs`.

## Top-level shape

```yaml
pollers:
  enabled: true
  state_db: ./data/poller.db
  default_jitter_ms: 5000
  lease_ttl_factor: 2.0
  failure_alert_cooldown_secs: 3600
  breaker_threshold: 5
  jobs:
    - id: ana_leads
      kind: gmail
      agent: ana
      schedule: { every_secs: 60 }
      config:
        query: "is:unread subject:lead"
        deliver: { channel: whatsapp, to: "57300...@s.whatsapp.net" }
        message_template: |
          New lead 🚨
          {snippet}
```

Absent file → subsystem off (no jobs spawn, no admin endpoint).

## Top-level fields

| Field | Default | Purpose |
|-------|---------|---------|
| `enabled` | `true` | Master switch. `false` skips everything below. |
| `state_db` | `./data/poller.db` | SQLite path for `poll_state` + `poll_lease`. Created if missing. |
| `default_jitter_ms` | `5000` | Random offset added to `next_run_at` when a job's schedule does not declare its own. Avoids thundering herd. |
| `lease_ttl_factor` | `2.0` | Lease TTL = `factor × interval` (min 30s). A daemon that crashes mid-tick releases the lease via expiry; another worker takes over without rerunning side effects unless your module is non-idempotent. |
| `failure_alert_cooldown_secs` | `3600` | Per-job cooldown for `failure_to` alerts. Persisted in `poll_state.last_failure_alert_at` so it survives restarts. |
| `breaker_threshold` | `5` | Consecutive `Transient` errors before the per-job circuit breaker opens. |
| `jobs` | `[]` | Per-job entries (see below). |

## Per-job fields

| Field | Required | Purpose |
|-------|:-------:|---------|
| `id` | ✅ | Unique. Used as session key for state, metrics, admin endpoints, lease. |
| `kind` | ✅ | Discriminator. Must match a registered `Poller::kind()` (see [Built-ins](#built-ins) and [Build a poller](../recipes/build-a-poller.md)). |
| `agent` | ✅ | Agent whose Phase 17 credentials this job uses. The runner looks up the binding for whatever channel the module needs (Google for fetch, WhatsApp/Telegram for outbound, etc). |
| `schedule` | ✅ | One of `every`, `cron`, `at` (see [Schedules](#schedules)). |
| `config` | — | Module-specific options. Validated by `Poller::validate` at boot. Bad config rejects this job only — siblings keep loading. |
| `failure_to` | — | `{ channel, to }` for an alert when consecutive_errors crosses `breaker_threshold`. Optional — omit to log only. |
| `paused_on_boot` | `false` | Persist `paused = 1` in state at startup. Useful for staged rollouts. |

## Schedules

```yaml
# Repeat every N seconds. Most common.
schedule: { every_secs: 60 }

# 6-field cron: sec min hour dom mon dow.
schedule:
  cron: "0 */5 * * * *"          # every 5 minutes on the boundary
  tz: "America/Bogota"           # accepted; evaluated in UTC unless cron-tz feature on
  stagger_jitter_ms: 2000        # local override for this job

# One-shot at an RFC3339 instant. After it fires the job stays paused.
schedule: { at: "2026-04-26T15:00:00Z" }
```

## Built-ins

| `kind` | Purpose | Cursor | Auth |
|--------|---------|--------|------|
| [`gmail`](#gmail) | Search Gmail, regex extract, dispatch | Reserved (Gmail UNREAD + mark_read does dedup) | Phase 17 Google |
| [`rss`](#rss) | RSS / Atom feeds | ETag + bounded seen-id ring | None |
| [`webhook_poll`](#webhook_poll) | Generic JSON GET / POST | Bounded seen-id ring | None / custom headers |
| [`google_calendar`](#google_calendar) | Calendar v3 events incremental sync | `nextSyncToken` | Phase 17 Google |

### `gmail`

```yaml
- id: ana_leads
  kind: gmail
  agent: ana
  schedule: { every_secs: 60 }
  config:
    query: "is:unread subject:(lead OR interesado)"
    newer_than: "1d"             # avoids back-filling years on first deploy
    max_per_tick: 20
    dispatch_delay_ms: 1000      # throttle between dispatches in same tick
    sender_allowlist: ["@mycompany.com"]
    extract:
      name: "Nombre:\\s*(.+)"
      phone: "Tel:\\s*(\\+?\\d+)"
    require_fields: [name, phone]
    message_template: |
      New lead 🚨 {name} — {phone}
      {snippet}
    mark_read_on_dispatch: true
    deliver: { channel: whatsapp, to: "57300...@s.whatsapp.net" }
```

Multiple gmail jobs for the same agent share a cached
`GoogleAuthClient` — token refreshes happen once across all jobs.

`google_*` errors are classified: 401 / `invalid_grant` / `revoked`
→ `Permanent` (auto-pause), 5xx / network → `Transient` (backoff).

### `rss`

```yaml
- id: ana_blog_watch
  kind: rss
  agent: ana
  schedule: { every_secs: 600 }
  config:
    feed_url: https://example.com/feed.xml
    max_per_tick: 5
    message_template: "{title}\n{link}"
    deliver: { channel: telegram, to: "1194292426" }
```

`ETag` from the previous response is sent as `If-None-Match`. `304
Not Modified` produces a zero-cost tick.

### `webhook_poll`

```yaml
- id: ana_jira_assigned
  kind: webhook_poll
  agent: ana
  schedule: { every_secs: 300 }
  config:
    url: https://company.atlassian.net/rest/api/3/search
    method: GET
    headers:
      Authorization: "Bearer ${JIRA_TOKEN}"
      Accept: "application/json"
    items_path: "issues"        # dotted path to the array; "" for root
    id_field: "id"              # field used for dedup
    max_per_tick: 10
    message_template: "[{key}] {fields}"
    deliver: { channel: telegram, to: "1194292426" }
    # SSRF guard — must opt in to hit private / loopback hosts:
    # allow_private_networks: true
```

`401` / `403` → `Permanent`. Any other 4xx → `Permanent`. 5xx →
`Transient`.

### `google_calendar`

```yaml
- id: ana_calendar_sync
  kind: google_calendar
  agent: ana
  schedule: { every_secs: 300 }
  config:
    calendar_id: primary
    skip_cancelled: true
    message_template: "📅 {summary} — {start}\n{html_link}"
    deliver: { channel: telegram, to: "1194292426" }
```

First tick captures `nextSyncToken` and dispatches nothing (baseline).
Subsequent ticks use `syncToken=...` and dispatch the diff. `410 Gone`
(token expired) is classified `Permanent` — operator runs
`agent pollers reset <id>` to re-baseline.

## Multi-job per built-in

Same agent + same kind, multiple jobs — completely independent. The
runner gives each its own cursor, breaker, schedule, metrics, and
pause/resume controls. The `GoogleAuthClient` is the only thing
shared (intentional, so quota and refresh costs aren't multiplied).

```yaml
# Three Gmail polls for Ana, all independent
- id: ana_leads
  kind: gmail
  agent: ana
  schedule: { every_secs: 60 }
  config:
    query: "is:unread label:lead"
    deliver: { channel: whatsapp, to: "57300...@s.whatsapp.net" }
    # …

- id: ana_invoices
  kind: gmail
  agent: ana
  schedule: { every_secs: 600 }
  config:
    query: "is:unread label:invoice"
    deliver: { channel: telegram, to: "1194292426" }
    # …

- id: ana_alerts
  kind: gmail
  agent: ana
  schedule: { cron: "0 */15 * * * *" }
  config:
    query: "is:unread from:monitor@infra.com"
    deliver: { channel: telegram, to: "9876543210" }
    # …
```

Pause `ana_invoices` independently with
`agent pollers pause ana_invoices`.

## CLI

```bash
agent pollers list                 # plain table; --json for machine output
agent pollers show ana_leads      # detail of one job
agent pollers run ana_leads       # manual tick (bypasses schedule + lease)
agent pollers pause ana_invoices  # paused = 1
agent pollers resume ana_invoices
agent pollers reset ana_calendar_sync --yes  # destructive; clears cursor
agent pollers reload              # re-read pollers.yaml + diff
```

The daemon must be running (CLI hits the loopback admin server at
`127.0.0.1:9091`).

## Admin endpoints

```
GET  /admin/pollers
GET  /admin/pollers/<id>
POST /admin/pollers/<id>/run
POST /admin/pollers/<id>/pause
POST /admin/pollers/<id>/resume
POST /admin/pollers/<id>/reset
POST /admin/pollers/reload
```

`reload` returns a `ReloadPlan` JSON: `{ add, replace, remove, keep }`.
Validation runs across every job in the new file before any task is
touched — a typo never knocks healthy siblings offline.

## Agent tools

When the poller subsystem is up, every agent gets six LLM-callable
tools registered on its `ToolRegistry`:

| Tool | Effect |
|------|--------|
| `pollers_list` | List every job + status |
| `pollers_show` | Inspect one job |
| `pollers_run` | Trigger a tick out-of-band |
| `pollers_pause` | Set `paused = 1` |
| `pollers_resume` | Set `paused = 0` |
| `pollers_reset` | Wipe cursor + errors (destructive) |

Each registered `Poller` impl can also expose **per-kind custom tools**
via `Poller::custom_tools()` — gmail ships `gmail_count_unread` out
of the box. See [Build a poller](../recipes/build-a-poller.md).

Create / delete are intentionally not exposed: prompt-injection
could plant a `webhook_poll` aimed at internal infra. Operators
own `pollers.yaml` + `agent pollers reload`.

## Failure-destination

```yaml
- id: ana_leads
  kind: gmail
  # …
  failure_to:
    channel: telegram
    to: "1194292426"     # alerts on the operator's chat
```

When the per-job circuit breaker trips
(`consecutive_errors >= breaker_threshold`), the runner publishes a
text message to the configured channel (resolved via Phase 17 just
like the happy path) and records the timestamp for cooldown
gating. Cooldown is `failure_alert_cooldown_secs` global default,
overridable per job in a future revision.

## Observability

Seven Prometheus series exposed under `/metrics`:

| Series | Type | Labels |
|--------|------|--------|
| `poller_ticks_total` | counter | `kind`, `agent`, `job_id`, `status={ok,transient,permanent,skipped}` |
| `poller_latency_ms` | histogram | `kind`, `agent`, `job_id` |
| `poller_items_seen_total` | counter | `kind`, `agent`, `job_id` |
| `poller_items_dispatched_total` | counter | `kind`, `agent`, `job_id` |
| `poller_consecutive_errors` | gauge | `job_id` |
| `poller_breaker_state` | gauge | `job_id` (`0=closed`, `1=half-open`, `2=open`) |
| `poller_lease_takeovers_total` | counter | `job_id` |

## Migrating from `gmail-poller.yaml`

The legacy crate `nexo-plugin-gmail-poller` keeps its YAML schema
but no longer drives its own loop. On boot the wizard
auto-translates every legacy job into a `kind: gmail` entry, folds
it into `cfg.pollers.jobs`, and logs a deprecation warn. Explicit
entries in `pollers.yaml` win on id collision so a manual migration
is never clobbered.

To migrate cleanly:

1. Run `agent --check-config` to print every translated id.
2. Copy each into `config/pollers.yaml` under `pollers.jobs`,
   adjusting the `agent:` field if the legacy `agent_id` was
   inferred.
3. Delete `config/plugins/gmail-poller.yaml`.
