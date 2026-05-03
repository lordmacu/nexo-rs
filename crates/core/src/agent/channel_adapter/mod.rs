//! Phase 81.8 — `ChannelAdapter` trait extension point.
//!
//! Lets plugins ship new channel kinds (SMS, Discord, IRC, Matrix,
//! custom webhooks) without modifying nexo-core. Plugins register
//! concrete adapters into [`ChannelAdapterRegistry`] (handle in
//! [`crate::agent::plugin_host::PluginInitContext`]) during
//! `NexoPlugin::init()`.
//!
//! The trait stays intentionally minimal — `kind() / start() /
//! stop() / send_outbound()`. Pairing / security / groups /
//! threading sub-traits land as Phase 81.8.x extensions when real
//! microapps demand them. Most channels need only the four
//! methods below.
//!
//! Conflict semantic: **first-registers-wins-rest-rejected** (NOT
//! first-plugin-wins like Phase 81.6/81.7). Channels compete for
//! broker topic exclusivity (`plugin.inbound.{kind}`); two plugins
//! registering the same kind would cause routing ambiguity, so the
//! second registration returns
//! [`ChannelAdapterRegistrationError::KindAlreadyRegistered`] and
//! a typed diagnostic is emitted.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use nexo_broker::AnyBroker;

/// Outbound shapes the agent runtime can dispatch to a channel.
/// `Custom` is the escape hatch for adapter-specific payloads
/// (Discord embeds, SMS template params, IRC notices); adapters
/// parse the JSON value against their internal type.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboundMessage {
    Text {
        to: String,
        body: String,
    },
    Media {
        to: String,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    Custom(serde_json::Value),
}

/// Minimal acknowledgement returned by
/// [`ChannelAdapter::send_outbound`]. Adapters that need richer
/// delivery semantics return a future-trait extension.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutboundAck {
    pub message_id: String,
    pub sent_at_unix: i64,
}

/// Failure modes for the lifecycle + send paths. Typed so callers
/// can discriminate retry-vs-fail-vs-fallback without parsing
/// strings.
#[derive(Debug, Error)]
pub enum ChannelAdapterError {
    #[error("channel adapter `{kind}` connection failure: {source}")]
    Connection {
        kind: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("channel adapter `{kind}` authentication failure: {reason}")]
    Authentication { kind: String, reason: String },
    #[error("channel adapter `{kind}` recipient `{recipient}` invalid: {reason}")]
    Recipient {
        kind: String,
        recipient: String,
        reason: String,
    },
    #[error("channel adapter `{kind}` rate-limited: retry after {retry_after_secs}s")]
    RateLimited { kind: String, retry_after_secs: u64 },
    #[error("channel adapter `{kind}` does not support: {feature}")]
    Unsupported { kind: String, feature: String },
    #[error("channel adapter `{kind}` error: {source}")]
    Other {
        kind: String,
        #[source]
        source: anyhow::Error,
    },
}

/// Phase 81.8 — extension trait that lets plugins ship new channel
/// kinds. Trait-object-safe (no `Self` returns, no async fn that
/// returns `impl Trait`). `'static` so adapters live for the
/// daemon lifetime.
#[async_trait]
pub trait ChannelAdapter: Send + Sync + 'static {
    /// Canonical channel kind. Matches the plugin manifest's
    /// `[plugin.channels.register].kind` entry.
    fn kind(&self) -> &str;

    /// Subscribe to the outbound topic + start publishing inbound
    /// events. Adapter is responsible for the topic shape:
    /// `plugin.outbound.{kind}` (or `.{kind}.{instance}` when
    /// `instance` is `Some`).
    async fn start(
        &self,
        broker: AnyBroker,
        instance: Option<&str>,
    ) -> Result<(), ChannelAdapterError>;

    /// Graceful stop — release resources, stop publishing inbound,
    /// drop broker subscriptions. Idempotent.
    async fn stop(&self) -> Result<(), ChannelAdapterError>;

    /// Send one outbound message. Returns a stamped ack on
    /// success; typed error on failure.
    async fn send_outbound(
        &self,
        msg: OutboundMessage,
    ) -> Result<OutboundAck, ChannelAdapterError>;
}

/// Process-wide registry of channel adapters. Boot-time only —
/// registration happens once during `NexoPlugin::init()`. Readers
/// (the agent runtime's outbound dispatcher) clone the
/// `Arc<dyn ChannelAdapter>` from the registry per send.
#[derive(Default)]
pub struct ChannelAdapterRegistry {
    inner: RwLock<BTreeMap<String, AdapterEntry>>,
}

impl std::fmt::Debug for ChannelAdapterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        f.debug_struct("ChannelAdapterRegistry")
            .field("kinds", &guard.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Clone)]
struct AdapterEntry {
    adapter: Arc<dyn ChannelAdapter>,
    /// Plugin id that registered the adapter. Used in the conflict
    /// diagnostic so the operator sees who got there first.
    registered_by: String,
}

impl ChannelAdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new adapter under its `kind()`. Returns
    /// `Err(KindAlreadyRegistered)` when another adapter already
    /// owns the kind. The plugin's other registrations
    /// (tools / advisors / hooks) are untouched.
    pub fn register(
        &self,
        adapter: Arc<dyn ChannelAdapter>,
        registered_by: impl Into<String>,
    ) -> Result<(), ChannelAdapterRegistrationError> {
        let kind = adapter.kind().to_string();
        let registered_by = registered_by.into();
        let mut guard = self.inner.write().unwrap_or_else(|p| p.into_inner());
        match guard.get(&kind) {
            Some(prior) => Err(
                ChannelAdapterRegistrationError::KindAlreadyRegistered {
                    kind,
                    prior_registered_by: prior.registered_by.clone(),
                    attempted_by: registered_by,
                },
            ),
            None => {
                guard.insert(
                    kind,
                    AdapterEntry {
                        adapter,
                        registered_by,
                    },
                );
                Ok(())
            }
        }
    }

    /// Lookup by kind. Cheap clone of the `Arc`. `None` when no
    /// plugin has registered the kind yet.
    pub fn get(&self, kind: &str) -> Option<Arc<dyn ChannelAdapter>> {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard.get(kind).map(|e| e.adapter.clone())
    }

    /// Sorted list of registered kinds. Stable iteration for
    /// admin-ui + doctor CLI rendering.
    pub fn kinds(&self) -> Vec<String> {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard.keys().cloned().collect()
    }

    /// `true` when at least one adapter is registered.
    pub fn has_any(&self) -> bool {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        !guard.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum ChannelAdapterRegistrationError {
    #[error(
        "channel adapter kind `{kind}` already registered by plugin `{prior_registered_by}` (attempted by `{attempted_by}`)"
    )]
    KindAlreadyRegistered {
        kind: String,
        prior_registered_by: String,
        attempted_by: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No-op mock adapter — the trait is satisfied without doing
    /// any real network IO. Each method records nothing; tests
    /// look at the registry, not adapter side effects.
    struct DummyAdapter {
        kind: &'static str,
    }

    impl DummyAdapter {
        fn new(kind: &'static str) -> Arc<Self> {
            Arc::new(Self { kind })
        }
    }

    #[async_trait]
    impl ChannelAdapter for DummyAdapter {
        fn kind(&self) -> &str {
            self.kind
        }
        async fn start(
            &self,
            _broker: AnyBroker,
            _instance: Option<&str>,
        ) -> Result<(), ChannelAdapterError> {
            Ok(())
        }
        async fn stop(&self) -> Result<(), ChannelAdapterError> {
            Ok(())
        }
        async fn send_outbound(
            &self,
            _msg: OutboundMessage,
        ) -> Result<OutboundAck, ChannelAdapterError> {
            Ok(OutboundAck {
                message_id: "dummy".into(),
                sent_at_unix: 0,
            })
        }
    }

    #[test]
    fn register_first_succeeds() {
        let reg = ChannelAdapterRegistry::new();
        assert!(!reg.has_any());
        let r = reg.register(DummyAdapter::new("sms"), "plugin_a");
        assert!(r.is_ok());
        assert!(reg.has_any());
        assert_eq!(reg.kinds(), vec!["sms".to_string()]);
        assert!(reg.get("sms").is_some());
    }

    #[test]
    fn register_duplicate_kind_rejected() {
        let reg = ChannelAdapterRegistry::new();
        reg.register(DummyAdapter::new("sms"), "plugin_a").unwrap();
        let err = reg
            .register(DummyAdapter::new("sms"), "plugin_b")
            .expect_err("duplicate must fail");
        match err {
            ChannelAdapterRegistrationError::KindAlreadyRegistered {
                kind,
                prior_registered_by,
                attempted_by,
            } => {
                assert_eq!(kind, "sms");
                assert_eq!(prior_registered_by, "plugin_a");
                assert_eq!(attempted_by, "plugin_b");
            }
        }
        // First registration still alive.
        assert!(reg.get("sms").is_some());
        // Map size unchanged.
        assert_eq!(reg.kinds(), vec!["sms".to_string()]);
    }

    #[test]
    fn kinds_returns_sorted_list() {
        let reg = ChannelAdapterRegistry::new();
        reg.register(DummyAdapter::new("zeta"), "p1").unwrap();
        reg.register(DummyAdapter::new("alpha"), "p2").unwrap();
        reg.register(DummyAdapter::new("mike"), "p3").unwrap();
        assert_eq!(
            reg.kinds(),
            vec!["alpha".to_string(), "mike".to_string(), "zeta".to_string()]
        );
    }

    #[test]
    fn get_unknown_kind_returns_none() {
        let reg = ChannelAdapterRegistry::new();
        assert!(reg.get("anything").is_none());
        reg.register(DummyAdapter::new("sms"), "p1").unwrap();
        assert!(reg.get("nope").is_none());
    }

    #[test]
    fn arc_clone_shares_registry_state() {
        let reg = Arc::new(ChannelAdapterRegistry::new());
        let clone_a = Arc::clone(&reg);
        let clone_b = Arc::clone(&reg);
        clone_a
            .register(DummyAdapter::new("discord"), "plugin_a")
            .unwrap();
        // clone_b sees the registration.
        assert!(clone_b.get("discord").is_some());
        assert_eq!(clone_b.kinds(), vec!["discord".to_string()]);
    }

    #[test]
    fn outbound_message_serde_round_trip() {
        let cases = vec![
            OutboundMessage::Text {
                to: "+57111".into(),
                body: "hi".into(),
            },
            OutboundMessage::Media {
                to: "+57222".into(),
                url: "https://x/y.png".into(),
                caption: Some("alt".into()),
            },
            OutboundMessage::Custom(
                serde_json::json!({"discord_embed": {"title": "x"}}),
            ),
        ];
        for case in cases {
            let s = serde_json::to_string(&case).unwrap();
            let back: OutboundMessage = serde_json::from_str(&s).unwrap();
            // Round-trip equivalence by serializing again — Eq is
            // not derivable on serde_json::Value, so compare JSON.
            assert_eq!(serde_json::to_string(&back).unwrap(), s);
        }

        let ack = OutboundAck {
            message_id: "m1".into(),
            sent_at_unix: 1700000000,
        };
        let s = serde_json::to_string(&ack).unwrap();
        let back: OutboundAck = serde_json::from_str(&s).unwrap();
        assert_eq!(back.message_id, "m1");
        assert_eq!(back.sent_at_unix, 1700000000);
    }
}
