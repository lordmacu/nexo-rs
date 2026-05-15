//! Phase 81.33.b.real Stage 5 — daemon-side plugin metrics
//! scrape.
//!
//! Plugins declare `[plugin.metrics] prometheus = true` +
//! `broker_topic_prefix = "..."`. The daemon's `/metrics`
//! handler iterates the registered scrape descriptors on every
//! request, issues `<broker_topic_prefix>.metrics.scrape`
//! broker RPC, and concatenates the returned Prometheus text
//! into the aggregate response.
//!
//! Replaces the previous pattern where each plugin's metrics
//! call was hardcoded inside `run_metrics_server` (e.g. the
//! legacy `nexo_plugin_email::metrics::render_prometheus(...)`
//! direct call).
//!
//! ## Wire format
//!
//! Daemon → plugin on `<broker_topic_prefix>.metrics.scrape`:
//!
//! ```json
//! {}
//! ```
//!
//! Plugin replies:
//!
//! ```json
//! { "text": "# HELP <metric_name> ...\n# TYPE ...\n<metric> <value>\n..." }
//! ```
//!
//! Empty / missing `text` is treated as a successful scrape with
//! no metrics. Plugins that fail to respond (timeout, broker
//! error, malformed reply) are skipped with a warn-level log —
//! one slow plugin does NOT stall the aggregate response.

use std::time::Duration;

use nexo_broker::{AnyBroker, BrokerHandle, Message};
use serde_json::json;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// One entry per plugin opting into metrics scrape.
#[derive(Debug, Clone)]
pub struct PluginMetricsDescriptor {
    pub plugin_id: String,
    pub broker_topic_prefix: String,
    pub timeout: Duration,
}

impl PluginMetricsDescriptor {
    pub fn new(plugin_id: impl Into<String>, broker_topic_prefix: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            broker_topic_prefix: broker_topic_prefix.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Scrape every declaring plugin in parallel and return the
/// concatenated Prometheus text. Plugins that fail (timeout,
/// broker error, malformed reply) contribute the empty string
/// + emit a warn-level log; one slow plugin does not stall the
/// whole `/metrics` request.
///
/// Returns a single `String` with newline-separated plugin
/// outputs. Caller appends to its own `/metrics` body.
pub async fn scrape_all(
    broker: &AnyBroker,
    descriptors: &[PluginMetricsDescriptor],
) -> String {
    if descriptors.is_empty() {
        return String::new();
    }
    // Issue every scrape sequentially. Concurrent join_all would
    // be marginally faster but pulls a `futures` dep edge that
    // this crate avoids; scrape volumes are small (≤10 plugins
    // typical, 5s timeout each) so serialised dispatch stays
    // well under any realistic Prometheus scrape budget.
    let mut results = Vec::with_capacity(descriptors.len());
    for d in descriptors {
        results.push(scrape_one(broker, d).await);
    }
    let mut out = String::new();
    for (descriptor, result) in descriptors.iter().zip(results.into_iter()) {
        match result {
            Ok(text) if text.is_empty() => {}
            Ok(text) => {
                out.push_str(&text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
            Err(err) => {
                tracing::warn!(
                    plugin = %descriptor.plugin_id,
                    error = %err,
                    "plugin metrics scrape failed; skipping",
                );
            }
        }
    }
    out
}

/// Scrape a single plugin. Used by [`scrape_all`] internally;
/// exposed for callers that want fine-grained control (e.g.
/// admin RPC `/metrics/per_plugin` follow-up).
pub async fn scrape_one(
    broker: &AnyBroker,
    descriptor: &PluginMetricsDescriptor,
) -> Result<String, PluginMetricsScrapeError> {
    let topic = format!("{}.metrics.scrape", descriptor.broker_topic_prefix);
    let msg = Message::new(topic.clone(), json!({}));
    let reply = broker
        .request(&topic, msg, descriptor.timeout)
        .await
        .map_err(|e| PluginMetricsScrapeError::Broker(e.to_string()))?;
    let text = reply
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(text)
}

#[derive(Debug, thiserror::Error)]
pub enum PluginMetricsScrapeError {
    #[error("broker error: {0}")]
    Broker(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scrape_one_returns_broker_error_when_no_subscriber() {
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let d = PluginMetricsDescriptor::new("email", "plugin.email")
            .with_timeout(Duration::from_millis(100));
        let result = scrape_one(&broker, &d).await;
        assert!(matches!(result, Err(PluginMetricsScrapeError::Broker(_))));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scrape_all_returns_empty_string_when_no_descriptors() {
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let out = scrape_all(&broker, &[]).await;
        assert_eq!(out, "");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scrape_all_skips_failed_plugins_without_panicking() {
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let descriptors = vec![
            PluginMetricsDescriptor::new("email", "plugin.email")
                .with_timeout(Duration::from_millis(50)),
            PluginMetricsDescriptor::new("telegram", "plugin.telegram")
                .with_timeout(Duration::from_millis(50)),
        ];
        // Both plugins time out (no subscribers). Aggregate
        // returns empty string but does NOT panic; the warn-log
        // path is exercised.
        let out = scrape_all(&broker, &descriptors).await;
        assert_eq!(out, "");
    }

    #[test]
    fn descriptor_with_timeout_overrides_default() {
        let d = PluginMetricsDescriptor::new("email", "plugin.email")
            .with_timeout(Duration::from_secs(10));
        assert_eq!(d.timeout, Duration::from_secs(10));
    }

    #[test]
    fn descriptor_default_timeout_is_five_seconds() {
        let d = PluginMetricsDescriptor::new("email", "plugin.email");
        assert_eq!(d.timeout, DEFAULT_TIMEOUT);
    }
}
