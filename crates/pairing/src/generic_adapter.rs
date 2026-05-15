//! Phase 81.33.b.real — broker-dispatched
//! [`PairingChannelAdapter`] for subprocess plugins.
//!
//! Plugins declare `[plugin.pairing.adapter]` in their
//! `nexo-plugin.toml` instead of shipping a hardcoded
//! `Arc<XxxPairingAdapter>` constructor that the daemon would have
//! to know by Rust type. The daemon constructs one
//! `GenericBrokerPairingAdapter` per declaring plugin and registers
//! it into the shared [`crate::registry::PairingAdapterRegistry`].
//!
//! ## Dispatch contract
//!
//! Daemon publishes JSON-RPC `request` messages on
//! `<broker_topic_prefix>.pairing.<method>`. Plugin subscribes,
//! handles, replies. All payloads are JSON.
//!
//! - `<prefix>.pairing.normalize_sender`
//!   request: `{ "raw": "<raw-sender>" }`
//!   reply:   `{ "normalized": "<canonical>" }`
//!            or `{ "normalized": null }` to reject (gate treats
//!            as `Decision::Drop`).
//! - `<prefix>.pairing.send_reply`
//!   request: `{ "account": "<inst>", "to": "<sender>", "text": "<challenge>" }`
//!   reply:   `{ "ok": true }` or `{ "ok": false, "error": "<message>" }`.
//! - `<prefix>.pairing.send_qr_image`
//!   request: `{ "account": "<inst>", "to": "<sender>", "png_base64": "<base64>" }`
//!   reply:   `{ "ok": true }` or `{ "ok": false, "error": "<message>" }`.
//!
//! ## `normalize_sender` cache
//!
//! [`PairingChannelAdapter::normalize_sender`] is a sync trait
//! method that the gate calls on every inbound message. A broker
//! round-trip per call would add ≥1ms to the inbound hot path.
//! The adapter therefore caches `(raw → Option<normalized>)` in
//! memory. Cache misses block via
//! [`tokio::task::block_in_place`] + [`tokio::runtime::Handle::block_on`],
//! so the caller must be on a multi-threaded tokio runtime — every
//! daemon hot path satisfies this.
//!
//! With `normalize_cache_ttl_seconds = None` (manifest default)
//! the cache lives the daemon's lifetime; pairing flows are
//! low-volume so unbounded growth is bounded by the operator's
//! distinct-sender count (typically < 10⁴).
//!
//! ## Channel-id intern
//!
//! [`PairingChannelAdapter::channel_id`] returns `&'static str`.
//! The adapter leaks the manifest-supplied `channel_id` once at
//! construction via [`Box::leak`] so the static reference is
//! valid for the daemon's lifetime. Bounded by `O(plugin count)`,
//! which is fixed at boot.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nexo_broker::{AnyBroker, BrokerHandle, Message};
use serde_json::json;

use crate::adapter::PairingChannelAdapter;

/// Default broker request timeout. Plugins that need longer
/// processing (e.g. QR image generation in the subprocess) get
/// 30s. Normalize calls usually return in milliseconds.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Cache entry. `value = None` means the plugin rejected the
/// sender (gate treats as Drop); cached because repeated
/// rejections shouldn't burn broker round-trips.
struct CacheEntry {
    value: Option<String>,
    inserted_at: Instant,
}

/// Manifest-driven broker-dispatched pairing adapter. See module
/// doc for the JSON-RPC contract.
pub struct GenericBrokerPairingAdapter {
    /// Interned `'static` channel id supplied to the registry
    /// under `register(adapter)`. Leaked at construction.
    channel_id: &'static str,
    /// Broker handle the adapter calls into. `Clone` because
    /// `AnyBroker` is itself a thin enum over `Arc`'d backends.
    broker: AnyBroker,
    /// Broker subject prefix from `[plugin.pairing.adapter]
    /// broker_topic_prefix = "..."`.
    topic_prefix: String,
    /// `(raw → Option<normalized>)` cache. `None` value =
    /// confirmed-reject (gate drops without re-asking the plugin).
    normalize_cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
    /// Cache TTL. `None` = unbounded (entries live forever);
    /// `Some(secs)` = evict on access when older than `secs`.
    normalize_ttl: Option<Duration>,
}

impl GenericBrokerPairingAdapter {
    /// Construct from the manifest section. `channel_id` is
    /// leaked to `&'static str` so the trait's `channel_id()`
    /// method can return without lifetime gymnastics.
    pub fn from_manifest(
        broker: AnyBroker,
        adapter: &nexo_plugin_manifest::pairing::PairingAdapterSection,
    ) -> Self {
        let leaked: &'static str =
            Box::leak(adapter.channel_id.clone().into_boxed_str());
        Self {
            channel_id: leaked,
            broker,
            topic_prefix: adapter.broker_topic_prefix.clone(),
            normalize_cache: Arc::new(RwLock::new(HashMap::new())),
            normalize_ttl: adapter.normalize_cache_ttl_seconds.map(Duration::from_secs),
        }
    }

    /// Lookup a cached entry, honouring TTL. Returns `None` if
    /// absent OR expired (caller treats as miss).
    fn cache_lookup(&self, raw: &str) -> Option<Option<String>> {
        let guard = self.normalize_cache.read().ok()?;
        let entry = guard.get(raw)?;
        if let Some(ttl) = self.normalize_ttl {
            if entry.inserted_at.elapsed() > ttl {
                return None;
            }
        }
        Some(entry.value.clone())
    }

    fn cache_insert(&self, raw: String, value: Option<String>) {
        if let Ok(mut guard) = self.normalize_cache.write() {
            guard.insert(
                raw,
                CacheEntry {
                    value,
                    inserted_at: Instant::now(),
                },
            );
        }
    }

    /// Block on `fut` from a sync context. Requires multi-threaded
    /// tokio runtime; panics otherwise (acceptable — every daemon
    /// hot path runs on multi-thread).
    fn block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(fut)
        })
    }
}

#[async_trait]
impl PairingChannelAdapter for GenericBrokerPairingAdapter {
    fn channel_id(&self) -> &'static str {
        self.channel_id
    }

    fn normalize_sender(&self, raw: &str) -> Option<String> {
        // Cache hit: cloned Option, no broker round-trip.
        if let Some(cached) = self.cache_lookup(raw) {
            return cached;
        }
        let topic = format!("{}.pairing.normalize_sender", self.topic_prefix);
        let payload = json!({ "raw": raw });
        let msg = Message::new(topic.clone(), payload);
        let result = self.block_on(
            self.broker.request(&topic, msg, DEFAULT_REQUEST_TIMEOUT),
        );
        let normalized = match result {
            Ok(reply) => reply
                .payload
                .get("normalized")
                .and_then(|v| match v {
                    serde_json::Value::Null => None,
                    serde_json::Value::String(s) => Some(s.clone()),
                    _ => None,
                }),
            Err(err) => {
                tracing::warn!(
                    channel = %self.channel_id,
                    raw = %raw,
                    error = %err,
                    "normalize_sender broker request failed; treating as reject",
                );
                None
            }
        };
        self.cache_insert(raw.to_string(), normalized.clone());
        normalized
    }

    async fn send_reply(&self, account: &str, to: &str, text: &str) -> anyhow::Result<()> {
        let topic = format!("{}.pairing.send_reply", self.topic_prefix);
        let payload = json!({
            "account": account,
            "to": to,
            "text": text,
        });
        let msg = Message::new(topic.clone(), payload);
        let reply = self
            .broker
            .request(&topic, msg, DEFAULT_REQUEST_TIMEOUT)
            .await
            .map_err(|e| anyhow::anyhow!("pairing.send_reply broker error: {e}"))?;
        let ok = reply
            .payload
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !ok {
            let err_msg = reply
                .payload
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("plugin reported send_reply failure without an error string");
            anyhow::bail!("pairing.send_reply: {}", err_msg);
        }
        Ok(())
    }

    async fn send_qr_image(
        &self,
        account: &str,
        to: &str,
        png: &[u8],
    ) -> anyhow::Result<()> {
        use base64::Engine;
        let topic = format!("{}.pairing.send_qr_image", self.topic_prefix);
        let payload = json!({
            "account": account,
            "to": to,
            "png_base64": base64::engine::general_purpose::STANDARD.encode(png),
        });
        let msg = Message::new(topic.clone(), payload);
        let reply = self
            .broker
            .request(&topic, msg, DEFAULT_REQUEST_TIMEOUT)
            .await
            .map_err(|e| anyhow::anyhow!("pairing.send_qr_image broker error: {e}"))?;
        let ok = reply
            .payload
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !ok {
            let err_msg = reply
                .payload
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("plugin reported send_qr_image failure without an error string");
            anyhow::bail!("pairing.send_qr_image: {}", err_msg);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_broker::LocalBroker;
    use nexo_plugin_manifest::pairing::PairingAdapterSection;

    fn make_section(prefix: &str) -> PairingAdapterSection {
        PairingAdapterSection {
            channel_id: "testchannel".into(),
            broker_topic_prefix: prefix.into(),
            format_challenge_text_kind: None,
            normalize_cache_ttl_seconds: None,
        }
    }

    fn make_adapter(broker: AnyBroker, prefix: &str) -> GenericBrokerPairingAdapter {
        GenericBrokerPairingAdapter::from_manifest(broker, &make_section(prefix))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn channel_id_is_leaked_from_manifest() {
        let broker = AnyBroker::Local(LocalBroker::new());
        let adapter = make_adapter(broker, "plugin.testchannel");
        assert_eq!(adapter.channel_id(), "testchannel");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normalize_sender_caches_reject_on_broker_failure() {
        // No subscriber on the topic → `LocalBroker::request`
        // returns a timeout, which the adapter treats as a reject
        // (`None` normalized). Confirms the cache populates even
        // on the failure path so repeated calls don't keep paying
        // the broker round-trip cost.
        let broker = AnyBroker::Local(LocalBroker::new());
        let adapter = make_adapter(broker, "plugin.testchannel");
        let result = adapter.normalize_sender("573001112222@c.us");
        assert!(result.is_none(), "no subscriber ⇒ reject");
        let cached = adapter.cache_lookup("573001112222@c.us");
        assert!(cached.is_some(), "cache populated on failure");
        assert!(cached.unwrap().is_none(), "cached value is reject");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normalize_sender_cache_hit_avoids_broker() {
        // Pre-populate cache with a successful normalize. Second
        // call returns the cached value without touching the
        // broker (no subscriber registered).
        let broker = AnyBroker::Local(LocalBroker::new());
        let adapter = make_adapter(broker, "plugin.testchannel");
        adapter.cache_insert("raw".into(), Some("canonical".into()));
        let result = adapter.normalize_sender("raw");
        assert_eq!(result.as_deref(), Some("canonical"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normalize_sender_cache_ttl_expires_entries() {
        let broker = AnyBroker::Local(LocalBroker::new());
        let mut section = make_section("plugin.testchannel");
        section.normalize_cache_ttl_seconds = Some(0);
        let adapter = GenericBrokerPairingAdapter::from_manifest(broker, &section);
        adapter.cache_insert("raw".into(), Some("normalized".into()));
        // TTL = 0 ⇒ any elapsed time looks expired.
        std::thread::sleep(Duration::from_millis(10));
        assert!(adapter.cache_lookup("raw").is_none());
    }
}
