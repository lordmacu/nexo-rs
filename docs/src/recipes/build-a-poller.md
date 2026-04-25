# Build a poller module

Three steps. No `main.rs` edit, no scheduler, no breaker, no SQLite
work. The runner gives you all of that — your code only describes
what to fetch, what to dispatch, and (optionally) what kind-specific
LLM tools to expose.

Reference: `crates/poller/src/builtins/` for in-tree examples (`gmail.rs`,
`rss.rs`, `webhook_poll.rs`, `google_calendar.rs`).

## Step 1 — implement the trait

```rust
// crates/poller/src/builtins/jira.rs
use std::sync::Arc;

use agent_poller::{
    OutboundDelivery, PollContext, Poller, PollerError, TickOutcome,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct JiraConfig {
    base_url: String,
    project_key: String,
    deliver: agent_poller::builtins::gmail::DeliverCfg,
}

pub struct JiraPoller;

#[async_trait]
impl Poller for JiraPoller {
    fn kind(&self) -> &'static str { "jira" }

    fn description(&self) -> &'static str {
        "Polls Jira for newly assigned issues in a project."
    }

    fn validate(&self, config: &Value) -> Result<(), PollerError> {
        serde_json::from_value::<JiraConfig>(config.clone())
            .map(drop)
            .map_err(|e| PollerError::Config {
                job: "<jira>".into(),
                reason: e.to_string(),
            })
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickOutcome, PollerError> {
        let cfg: JiraConfig = serde_json::from_value(ctx.config.clone())
            .map_err(|e| PollerError::Config {
                job: ctx.job_id.clone(),
                reason: e.to_string(),
            })?;

        // 1. Pull data. Use ctx.cursor for incremental fetches.
        // 2. Decide what to dispatch.
        // 3. Build OutboundDelivery items — the runner publishes them
        //    via Phase 17 credentials so you never touch the broker.

        let payload = json!({ "text": "(jira tick — replace with real fetch)" });
        Ok(TickOutcome {
            items_seen: 0,
            items_dispatched: 1,
            deliver: vec![OutboundDelivery {
                channel: agent_auth::handle::TELEGRAM,
                recipient: cfg.deliver.to.clone(),
                payload,
            }],
            next_cursor: None,
            next_interval_hint: None,
        })
    }
}
```

Anything `Poller::validate` returns `Err(PollerError::Config { … })`
fails this job at boot — siblings keep going.

`Poller::tick` returns:
- `Ok(TickOutcome)` — the runner persists `next_cursor`, increments
  counters, dispatches every `OutboundDelivery` via the agent's
  Phase 17 binding, and sleeps until next slot.
- `Err(PollerError::Transient(…))` — counts toward the breaker;
  next tick retries with backoff.
- `Err(PollerError::Permanent(…))` — auto-pauses the job and fires
  the `failure_to` alert.

`PollContext.stores` exposes the credential stores when your module
needs *paths* (e.g., Gmail / Calendar built-ins read
`client_id_path` from there). Plain `ctx.credentials.resolve(…)` is
enough when you only need a `CredentialHandle`.

## Step 2 — register

```rust
// crates/poller/src/builtins/mod.rs
pub mod gmail;
pub mod google_calendar;
pub mod jira;          // ← new
pub mod rss;
pub mod webhook_poll;

pub fn register_all(runner: &PollerRunner) {
    runner.register(Arc::new(gmail::GmailPoller::new()));
    runner.register(Arc::new(rss::RssPoller::new()));
    runner.register(Arc::new(webhook_poll::WebhookPoller::new()));
    runner.register(Arc::new(google_calendar::GoogleCalendarPoller::new()));
    runner.register(Arc::new(jira::JiraPoller));   // ← new
}
```

That is the only place wiring is touched. `main.rs` already calls
`register_all`.

## Step 3 — declare a job

```yaml
# config/pollers.yaml
pollers:
  jobs:
    - id: ana_jira_assigned
      kind: jira
      agent: ana
      schedule: { every_secs: 300 }
      config:
        base_url: https://company.atlassian.net
        project_key: ENG
        deliver:
          channel: telegram
          to: "1194292426"
```

Run the daemon. Verify with:

```bash
agent pollers list                # ana_jira_assigned shows up
agent pollers run ana_jira_assigned   # tick on demand
```

## Add per-kind LLM tools

Your module can ship its own tools alongside the generic
`pollers_*` ones. Override `Poller::custom_tools`:

```rust
fn custom_tools(&self) -> Vec<agent_poller::CustomToolSpec> {
    use agent_llm::ToolDef;
    use agent_poller::{CustomToolHandler, CustomToolSpec, PollerRunner};
    use async_trait::async_trait;

    struct JiraSearch;
    #[async_trait]
    impl CustomToolHandler for JiraSearch {
        async fn call(
            &self,
            runner: Arc<PollerRunner>,
            args: Value,
        ) -> anyhow::Result<Value> {
            // Use `runner` to inspect / mutate jobs the same way
            // built-in `pollers_*` tools do — list_jobs, run_once,
            // set_paused, reset_cursor are all available.
            let id = args["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("`id` required"))?;
            let outcome = runner.run_once(id).await?;
            Ok(json!({ "matching": outcome.items_seen }))
        }
    }

    vec![CustomToolSpec {
        def: ToolDef {
            name: "jira_search".into(),
            description: "Run the Jira poll job once without persisting state.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                },
                "required": ["id"]
            }),
        },
        handler: Arc::new(JiraSearch),
    }]
}
```

The agent then sees `jira_search` automatically — no extra
registration step. The adapter in
`agent-poller-tools::register_all` walks every registered Poller's
`custom_tools()` and wires each spec into the per-agent
`ToolRegistry`.

## What the runner gives you for free

- Per-job `tokio` task with `every | cron | at` schedule + jitter.
- Cross-process atomic lease in SQLite (lease takeover after TTL
  expiry — daemon crash mid-tick is recoverable).
- Cursor persistence — your `next_cursor` is the next tick's
  `ctx.cursor`. Survives restarts. `agent pollers reset <id>`
  clears it.
- Exponential backoff on `Transient`, auto-pause on `Permanent`.
- Per-job circuit breaker keyed on `("poller", job_id)`.
- Outbound dispatch via Phase 17 — `OutboundDelivery` lands at
  `plugin.outbound.<channel>.<instance>` resolved from the agent's
  binding. You never touch the broker.
- 7 Prometheus series labelled by `kind`, `agent`, `job_id`,
  `status`. Audit log under `target=credentials.audit`.
- Admin endpoints + CLI subcommands (`agent pollers …`).
- Six generic LLM tools (`pollers_list`, `pollers_show`,
  `pollers_run`, `pollers_pause`, `pollers_resume`,
  `pollers_reset`).
- Hot-reload via `POST /admin/pollers/reload` — `add | replace |
  remove | keep` plan applied atomically.

## Tests pattern

```rust
#[tokio::test]
async fn validate_accepts_minimal() {
    let p = JiraPoller;
    let cfg = json!({
        "base_url": "https://x.atlassian.net",
        "project_key": "ENG",
        "deliver": { "channel": "telegram", "to": "1" },
    });
    p.validate(&cfg).unwrap();
}

#[tokio::test]
async fn validate_rejects_unknown_field() {
    let p = JiraPoller;
    let cfg = json!({ "wat": true, "deliver": { "channel": "x", "to": "1" }});
    assert!(p.validate(&cfg).is_err());
}
```

Cursor / dispatch tests follow the same pattern as the in-tree
built-ins (`gmail.rs`, `rss.rs`, `webhook_poll.rs`).

## Anti-patterns

- **Don't publish to the broker directly from `tick`.** Return
  `OutboundDelivery` so the runner uses Phase 17 + audit log.
- **Don't share global state across modules.** Use cursors for
  per-job state; use `DashMap` inside your struct for per-account
  caches (gmail does this for `GoogleAuthClient`).
- **Don't sleep inside `tick` for backoff.** Return
  `PollerError::Transient` and let the runner own the backoff
  schedule — that way `agent pollers reset` and hot-reload still
  cancel cleanly.
- **Don't auto-create jobs from inside an LLM tool.** The runner
  intentionally exposes only read + control on existing jobs.
  Operators own `pollers.yaml`.
