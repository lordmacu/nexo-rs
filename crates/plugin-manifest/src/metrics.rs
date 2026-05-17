//! Phase 81.33.b.real Stage 5 — `[plugin.metrics]` manifest
//! section.
//!
//! Plugins exposing Prometheus metrics declare a broker topic
//! the daemon scrapes on every `/metrics` request. The plugin's
//! subprocess handles the scrape, returns Prometheus text, and
//! the daemon concatenates it into the aggregate response.
//!
//! Replaces the previous pattern where each plugin had a
//! hardcoded `nexo_plugin_X::metrics::render_prometheus(...)`
//! call inside the daemon's metrics handler.

use serde::{Deserialize, Serialize};

/// Daemon-side Prometheus metrics declaration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginMetricsSection {
    /// `true` opts the plugin into the `/metrics` scrape loop.
    /// Daemon issues `<broker_topic_prefix>.metrics.scrape` per
    /// scrape request; plugin replies with Prometheus text.
    /// `false` (default) keeps the plugin out of the aggregate.
    #[serde(default)]
    pub prometheus: bool,

    /// Broker subject prefix for the scrape RPC. Daemon publishes
    /// to `<broker_topic_prefix>.metrics.scrape`. Required when
    /// `prometheus = true`.
    #[serde(default)]
    pub broker_topic_prefix: String,

    /// Per-scrape broker RPC timeout. Default 5s — scrapes happen
    /// per `/metrics` HTTP request so the daemon-side latency
    /// budget is tight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl PluginMetricsSection {
    pub fn validate(&self) -> Result<(), String> {
        if self.prometheus && self.broker_topic_prefix.is_empty() {
            return Err("broker_topic_prefix cannot be empty when prometheus = true".into());
        }
        if self.broker_topic_prefix.contains(' ') {
            return Err("broker_topic_prefix cannot contain spaces".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_minimal() {
        let s = PluginMetricsSection {
            prometheus: true,
            broker_topic_prefix: "plugin.email".into(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_rejects_prometheus_without_topic() {
        let s = PluginMetricsSection {
            prometheus: true,
            broker_topic_prefix: String::new(),
            timeout_seconds: None,
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_accepts_prometheus_off_without_topic() {
        // prometheus=false with empty topic is fine — explicit opt-out.
        let s = PluginMetricsSection::default();
        assert!(s.validate().is_ok());
    }

    #[test]
    fn deserializes_minimal_toml() {
        let toml = r#"
            prometheus          = true
            broker_topic_prefix = "plugin.email"
        "#;
        let s: PluginMetricsSection = toml::from_str(toml).expect("parse");
        assert!(s.prometheus);
        assert_eq!(s.broker_topic_prefix, "plugin.email");
    }
}
