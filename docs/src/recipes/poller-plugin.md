# Build a poller plugin (V2 — out-of-tree subprocess)

Phase 96 introduced the `[plugin.poller]` manifest section. Out-of-tree
poller plugins ship as standalone Cargo crates publishing to crates.io,
spawned as subprocesses by the daemon, and communicating with the
runtime via broker JSON-RPC. The daemon's `nexo-poller` runtime stays
provider-agnostic — pollers reach the world through a single egress
trait (`PollerHost`) for outbound, credentials, logs, metrics, and LLM
invocations.

If you maintained an in-tree builtin under `crates/poller/src/builtins/`
before Phase 96, migrate to this recipe. The legacy
`nexo-poller-ext` `StdioRuntime` bridge is deprecated since v0.2.0 and
slated for deletion two release cycles after Phase 96 ships.

## Three steps

1. Scaffold a new Cargo crate that depends on `nexo-microapp-sdk` with
   the `poller` feature.
2. Implement `PollerHandler::tick` — fetch, parse, dispatch via
   `host.broker_publish`, return a `TickAck`.
3. Write a `nexo-plugin.toml` declaring `[plugin.poller]` plus the
   broker topics your plugin needs to subscribe / publish on.

## Step 1 — Cargo.toml

```toml
[package]
name = "nexo-poller-jira"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "nexo-poller-jira"
path = "src/main.rs"

[lib]
name = "nexo_poller_jira"
path = "src/lib.rs"

[dependencies]
nexo-microapp-sdk = { version = "0.2", features = ["plugin", "poller"] }
nexo-poller       = "0.2"
nexo-broker       = "0.1"
nexo-config       = "0.1"

tokio              = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time", "io-util", "io-std"] }
async-trait        = "0.1"
serde              = { version = "1", features = ["derive"] }
serde_json         = "1"
anyhow             = "1"
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
reqwest            = { version = "0.12", default-features = false, features = ["rustls-tls"] }
```

## Step 2 — `PollerHandler` implementation

```rust
// src/lib.rs
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use nexo_microapp_sdk::poller::{PollerHandler, TickRequest};
use nexo_poller::{PollerError, PollerHost, TickAck, TickMetrics};

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct JiraJobConfig {
    pub base_url: String,
    pub project_key: String,
    pub deliver: DeliverCfg,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DeliverCfg {
    pub channel: String,
    #[serde(alias = "recipient")]
    pub to: String,
}

pub struct JiraHandler {
    http: reqwest::Client,
}

impl JiraHandler {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest"),
        }
    }
}

#[async_trait]
impl PollerHandler for JiraHandler {
    async fn tick(
        &self,
        req: TickRequest,
        host: Arc<dyn PollerHost>,
    ) -> Result<TickAck, PollerError> {
        let cfg: JiraJobConfig = serde_json::from_value(req.config.clone())
            .map_err(|e| PollerError::Config { job: req.job_id.clone(), reason: e.to_string() })?;

        // Fetch from Jira (replace with real API call).
        let resp = self.http.get(&format!("{}/rest/api/3/search?jql=project={}", cfg.base_url, cfg.project_key))
            .send().await
            .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
        if !resp.status().is_success() {
            return Err(PollerError::Transient(anyhow::anyhow!("HTTP {}", resp.status())));
        }
        let _body: serde_json::Value = resp.json().await
            .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;

        // Resolve the outbound channel's account_id via reverse-RPC.
        let cred = host.credentials_get(cfg.deliver.channel.clone()).await
            .map_err(|e| PollerError::Permanent(anyhow::anyhow!("credentials_get: {e}")))?;
        let account_id = cred.get("account_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PollerError::Permanent(anyhow::anyhow!("no account_id")))?
            .to_string();
        let topic = format!("plugin.outbound.{}.{}", cfg.deliver.channel, account_id);

        // Dispatch one message per new issue.
        let payload = json!({ "to": cfg.deliver.to, "text": "new Jira issue" });
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
        host.broker_publish(topic, payload_bytes).await
            .map_err(|e| PollerError::Transient(anyhow::anyhow!("broker_publish: {e}")))?;

        Ok(TickAck {
            next_cursor: None,
            next_interval_hint: None,
            metrics: Some(TickMetrics { items_seen: 1, items_dispatched: 1 }),
        })
    }
}
```

## Step 3 — `nexo-plugin.toml`

```toml
manifest_version = 2

[plugin]
id               = "jira"
version          = "0.1.0"
name             = "Jira Poller"
description      = "Jira issues poller — fetches new issues, dispatches via deliver channel."
min_nexo_version = ">=0.2.0"

[plugin.entrypoint]
command = "nexo-poller-jira"

[plugin.requires]
nexo_capabilities = ["broker"]

[plugin.capabilities.broker]
subscribe = [
    "plugin.poller.jira.tick",
    "_inbox.>",
]
publish = [
    "daemon.rpc.jira",
    "plugin.outbound.whatsapp.>",
    "plugin.outbound.telegram.>",
    "_inbox.>",
]

[plugin.poller]
kinds                = ["jira"]
broker_topic_prefix  = "plugin.poller.jira"
lifecycle            = "long_lived"
max_concurrent_ticks = 1
tick_timeout_secs    = 60
```

## Operator config

```yaml
# pollers.yaml
jobs:
  - id: backend_jira
    kind: jira
    agent: ana
    schedule: { every: 15m }
    config:
      base_url: "https://acme.atlassian.net"
      project_key: "ENG"
      deliver:
        channel: telegram
        to: "-1001234567"
```

## Install + boot

```bash
cargo install nexo-poller-jira
agent run
```

The daemon discovers the plugin via its `[plugin.entrypoint]` line,
registers the `jira` kind in the `PluginPollerRouter`, and routes
matching jobs through broker JSON-RPC. The plugin's broker subscriber
receives ticks on `plugin.poller.jira.tick`, dispatches to your
`PollerHandler::tick`, encodes the `TickAck` into the wire reply, and
publishes back on the message's `reply_to` topic.

## What `PollerHost` exposes

The poller reaches the runtime through one trait. Four methods:

| Method | Use case |
|--------|----------|
| `broker_publish(topic, payload)` | Outbound — direct to broker (Phase 92 path) |
| `credentials_get(channel)` | Resolve `{ account_id, … }` for the outbound channel |
| `log(level, message, fields)` | Structured log forwarded to daemon tracing |
| `metric_inc(name, labels)` | Counter increment forwarded to daemon Prometheus |
| `llm_invoke(request)` | LLM completion through daemon's `LlmRegistry` |

No `OutboundDelivery`, no `Channel` enum, no credential bundle types
in your code — your plugin owns its own outbound logic and topic
construction.

## Migrating from V1 (in-tree builtin)

If you maintained a builtin under `crates/poller/src/builtins/`:

1. Create the standalone repo from the recipe above.
2. Copy your `Poller::tick` body into `PollerHandler::tick`. Three
   rename rules:
   - `ctx.credentials.resolve(agent, channel)` → `host.credentials_get(channel).await`
   - `OutboundDelivery { channel, recipient, payload }` push → build the topic yourself
     (`plugin.outbound.<channel>.<account_id>`) and call `host.broker_publish(topic, payload_bytes)`
   - `TickOutcome { items_seen, items_dispatched, deliver, next_cursor, next_interval_hint }`
     → `TickAck { next_cursor, next_interval_hint, metrics: Some(TickMetrics { items_seen, items_dispatched }) }`
3. Drop your entry from `crates/poller/src/builtins/mod.rs::register_all`.
4. Publish your new crate to crates.io. The daemon's
   `[plugin.poller]` manifest discovery picks it up at boot.

The reference Phase 96 extractions live at
`nexo-rs-poller-rss`, `nexo-rs-poller-google-calendar`, and
`nexo-rs-poller-gmail` — see those repos for end-to-end examples
with broker subscriber boot, reverse-RPC credential refresh, and
serde-driven config parsing.
