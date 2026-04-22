use std::collections::HashMap;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    pub providers: HashMap<String, LlmProviderConfig>,
    #[serde(default)]
    pub retry: RetryConfig,
}

#[derive(Debug, Deserialize)]
pub struct LlmProviderConfig {
    pub api_key: String,
    pub base_url: String,
    pub group_id: Option<String>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

#[derive(Debug, Deserialize, Default)]
pub struct RateLimitConfig {
    #[serde(default = "default_rps")]
    pub requests_per_second: f32,
    pub quota_alert_threshold: Option<u64>,
}

fn default_rps() -> f32 { 2.0 }

#[derive(Debug, Deserialize, Default)]
pub struct RetryConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_initial_backoff")]
    pub initial_backoff_ms: u64,
    #[serde(default = "default_max_backoff")]
    pub max_backoff_ms: u64,
    #[serde(default = "default_multiplier")]
    pub backoff_multiplier: f32,
}

fn default_max_attempts() -> u32 { 5 }
fn default_initial_backoff() -> u64 { 1000 }
fn default_max_backoff() -> u64 { 60_000 }
fn default_multiplier() -> f32 { 2.0 }
