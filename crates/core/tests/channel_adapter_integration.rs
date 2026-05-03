//! Phase 81.8 — end-to-end ChannelAdapter registration + outbound
//! round-trip via a mock SMS adapter that tracks state transitions.
//! No real broker IO — the mock adapter's `start()` does not
//! subscribe to anything; this test exercises the trait + registry
//! contract only.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use nexo_broker::AnyBroker;
use nexo_core::agent::channel_adapter::{
    ChannelAdapter, ChannelAdapterError, ChannelAdapterRegistry,
    OutboundAck, OutboundMessage,
};

struct MockSmsAdapter {
    starts: AtomicUsize,
    stops: AtomicUsize,
    sends: AtomicUsize,
}

impl MockSmsAdapter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            sends: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl ChannelAdapter for MockSmsAdapter {
    fn kind(&self) -> &str {
        "sms"
    }
    async fn start(
        &self,
        _broker: AnyBroker,
        _instance: Option<&str>,
    ) -> Result<(), ChannelAdapterError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&self) -> Result<(), ChannelAdapterError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn send_outbound(
        &self,
        msg: OutboundMessage,
    ) -> Result<OutboundAck, ChannelAdapterError> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        let id = match msg {
            OutboundMessage::Text { .. } => "text-1",
            OutboundMessage::Media { .. } => "media-1",
            OutboundMessage::Custom(_) => "custom-1",
        };
        Ok(OutboundAck {
            message_id: id.into(),
            sent_at_unix: 1700000000,
        })
    }
}

#[tokio::test]
async fn channel_adapter_register_and_round_trip() {
    let registry = Arc::new(ChannelAdapterRegistry::new());
    let mock = MockSmsAdapter::new();

    // Register via the trait-object upcast.
    registry
        .register(mock.clone() as Arc<dyn ChannelAdapter>, "nexo-plugin-sms")
        .expect("first registration succeeds");

    assert_eq!(registry.kinds(), vec!["sms".to_string()]);
    assert!(registry.has_any());

    // Lookup + send.
    let resolved = registry.get("sms").expect("registered");
    let ack = resolved
        .send_outbound(OutboundMessage::Text {
            to: "+57111".into(),
            body: "hi".into(),
        })
        .await
        .expect("send ok");
    assert_eq!(ack.message_id, "text-1");
    assert_eq!(ack.sent_at_unix, 1700000000);

    // Send a media message via the same registered adapter.
    let _ = resolved
        .send_outbound(OutboundMessage::Media {
            to: "+57222".into(),
            url: "https://x/y.png".into(),
            caption: None,
        })
        .await
        .expect("media send ok");

    // Mock state reflects 2 sends.
    assert_eq!(mock.sends.load(Ordering::SeqCst), 2);
    assert_eq!(mock.starts.load(Ordering::SeqCst), 0);
    assert_eq!(mock.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn channel_adapter_duplicate_registration_rejected() {
    let registry = Arc::new(ChannelAdapterRegistry::new());
    let first = MockSmsAdapter::new();
    let second = MockSmsAdapter::new();

    registry
        .register(first as Arc<dyn ChannelAdapter>, "plugin_a")
        .expect("first ok");
    let err = registry
        .register(second as Arc<dyn ChannelAdapter>, "plugin_b")
        .expect_err("second must fail");
    let msg = format!("{err}");
    assert!(msg.contains("sms"));
    assert!(msg.contains("plugin_a"));
    assert!(msg.contains("plugin_b"));

    // Registry still has the first registration.
    assert_eq!(registry.kinds(), vec!["sms".to_string()]);
}
