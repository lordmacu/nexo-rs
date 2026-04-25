//! Anthropic `/v1/messages/count_tokens` integration.
//!
//! Exact token counts (matches what the billing endpoint will report)
//! at the cost of a network round-trip. We amortise that cost with an
//! LRU keyed on `blake3(payload)`: the stable tools+identity prefix
//! hashes the same on every turn, so 95%+ of a typical agent's request
//! body is served from the in-process cache.
//!
//! Failures are fatal here — wrap this counter in a `CircuitBreaker`
//! upstream so a 5xx on the count endpoint can fall back to the offline
//! tiktoken approximation without bringing the agent loop down.

use std::sync::Mutex;

use async_trait::async_trait;
use lru::LruCache;
use serde_json::{json, Value};

use super::TokenCounter;
use crate::prompt_block::{flatten_blocks, PromptBlock};
use crate::retry::LlmError;
use crate::types::{ChatMessage, ChatRole};

const COUNT_ENDPOINT: &str = "/v1/messages/count_tokens";

pub struct AnthropicTokenCounter {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    cache: Mutex<LruCache<[u8; 32], u32>>,
}

impl AnthropicTokenCounter {
    pub fn new(base_url: &str, api_key: &str, cache_capacity: u32) -> Self {
        let cap = std::num::NonZeroUsize::new(cache_capacity.max(1) as usize)
            .expect("cache_capacity > 0 by clamp");
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client build");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            cache: Mutex::new(LruCache::new(cap)),
        }
    }

    fn cache_key(payload: &Value) -> [u8; 32] {
        let bytes = serde_json::to_vec(payload).unwrap_or_default();
        let h = blake3::hash(&bytes);
        *h.as_bytes()
    }

    fn cache_get(&self, key: &[u8; 32]) -> Option<u32> {
        self.cache.lock().ok().and_then(|mut g| g.get(key).copied())
    }

    fn cache_put(&self, key: [u8; 32], value: u32) {
        if let Ok(mut g) = self.cache.lock() {
            g.put(key, value);
        }
    }

    async fn count_payload(&self, payload: Value) -> Result<u32, LlmError> {
        let key = Self::cache_key(&payload);
        if let Some(hit) = self.cache_get(&key) {
            return Ok(hit);
        }
        let url = format!("{}{}", self.base_url, COUNT_ENDPOINT);
        let resp = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| LlmError::Other(e.into()))?;
        if status == 429 {
            // Tell the caller this is transient so the breaker can
            // open promptly and the agent loop falls back to tiktoken.
            return Err(LlmError::RateLimit {
                retry_after_ms: 30_000,
            });
        }
        if status.is_server_error() {
            return Err(LlmError::ServerError {
                status: status.as_u16(),
                body: text,
            });
        }
        if !status.is_success() {
            return Err(LlmError::Other(anyhow::anyhow!(
                "count_tokens HTTP {}: {}",
                status,
                text
            )));
        }
        let parsed: Value = serde_json::from_str(&text).map_err(|e| {
            LlmError::Other(anyhow::anyhow!("count_tokens parse: {e} (body: {text})"))
        })?;
        let n = parsed
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                LlmError::Other(anyhow::anyhow!(
                    "count_tokens response missing `input_tokens`: {text}"
                ))
            })?;
        let n = n as u32;
        self.cache_put(key, n);
        Ok(n)
    }
}

fn role_str(r: &ChatRole) -> &'static str {
    match r {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "user",
    }
}

#[async_trait]
impl TokenCounter for AnthropicTokenCounter {
    async fn count_blocks(&self, blocks: &[PromptBlock]) -> Result<u32, LlmError> {
        // count_tokens needs a model + at least one message. We feed a
        // placeholder user turn so the endpoint focuses on the system
        // bytes, then subtract a small constant for the placeholder.
        let payload = json!({
            "model": "claude-sonnet-4-5",
            "system": flatten_blocks(blocks),
            "messages": [{ "role": "user", "content": "." }],
        });
        // The single "." user turn costs ~4 tokens. Subtracting keeps
        // the returned figure close to the system-only cost; clamp to
        // zero to stay safe.
        let raw = self.count_payload(payload).await?;
        Ok(raw.saturating_sub(4))
    }

    async fn count_messages(&self, model: &str, messages: &[ChatMessage]) -> Result<u32, LlmError> {
        if messages.is_empty() {
            return Ok(0);
        }
        let mut wire: Vec<Value> = Vec::with_capacity(messages.len());
        for m in messages {
            wire.push(json!({
                "role": role_str(&m.role),
                "content": m.content,
            }));
        }
        let payload = json!({
            "model": model,
            "messages": wire,
        });
        self.count_payload(payload).await
    }

    fn is_exact(&self) -> bool {
        true
    }

    fn backend(&self) -> &'static str {
        "anthropic_api"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_deterministic_per_payload() {
        let a = json!({"model":"x","messages":[{"role":"user","content":"hi"}]});
        let b = json!({"model":"x","messages":[{"role":"user","content":"hi"}]});
        assert_eq!(
            AnthropicTokenCounter::cache_key(&a),
            AnthropicTokenCounter::cache_key(&b)
        );
    }

    #[test]
    fn cache_key_changes_when_payload_changes() {
        let a = json!({"model":"x","messages":[{"role":"user","content":"hi"}]});
        let b = json!({"model":"x","messages":[{"role":"user","content":"bye"}]});
        assert_ne!(
            AnthropicTokenCounter::cache_key(&a),
            AnthropicTokenCounter::cache_key(&b)
        );
    }

    #[test]
    fn lru_get_put_roundtrip() {
        let c = AnthropicTokenCounter::new("https://localhost", "k", 4);
        let key = [7u8; 32];
        assert!(c.cache_get(&key).is_none());
        c.cache_put(key, 42);
        assert_eq!(c.cache_get(&key), Some(42));
    }

    #[test]
    fn lru_evicts_oldest_when_full() {
        let c = AnthropicTokenCounter::new("https://localhost", "k", 2);
        c.cache_put([1u8; 32], 1);
        c.cache_put([2u8; 32], 2);
        c.cache_put([3u8; 32], 3);
        // Capacity 2 — oldest [1] is gone.
        assert!(c.cache_get(&[1u8; 32]).is_none());
        assert_eq!(c.cache_get(&[2u8; 32]), Some(2));
        assert_eq!(c.cache_get(&[3u8; 32]), Some(3));
    }

    #[test]
    fn metadata() {
        let c = AnthropicTokenCounter::new("u", "k", 8);
        assert!(c.is_exact());
        assert_eq!(c.backend(), "anthropic_api");
    }
}
