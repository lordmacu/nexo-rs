//! Tests for `PairingAdapterRegistry`.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_pairing::adapter::PairingChannelAdapter;
use nexo_pairing::PairingAdapterRegistry;

struct MockAdapter {
    id: &'static str,
    tag: &'static str,
}

#[async_trait]
impl PairingChannelAdapter for MockAdapter {
    fn channel_id(&self) -> &'static str {
        self.id
    }
    fn normalize_sender(&self, raw: &str) -> Option<String> {
        Some(raw.to_string())
    }
    async fn send_reply(&self, _account: &str, _to: &str, _text: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

impl MockAdapter {
    fn tag(&self) -> &'static str {
        self.tag
    }
}

#[test]
fn register_then_get_returns_inserted_adapter() {
    let reg = PairingAdapterRegistry::new();
    let a = Arc::new(MockAdapter { id: "wa", tag: "first" });
    reg.register(a);
    let got = reg.get("wa").expect("adapter present");
    // Downcast via concrete trait method — channel_id is enough to
    // confirm the right adapter came back.
    assert_eq!(got.channel_id(), "wa");
}

#[test]
fn get_returns_none_for_unregistered_channel() {
    let reg = PairingAdapterRegistry::new();
    assert!(reg.get("telegram").is_none());
    assert!(reg.is_empty());
}

#[test]
fn double_register_overwrites_previous_entry() {
    let reg = PairingAdapterRegistry::new();
    reg.register(Arc::new(MockAdapter { id: "wa", tag: "first" }));
    reg.register(Arc::new(MockAdapter { id: "wa", tag: "second" }));
    let got = reg.get("wa").expect("adapter present");
    // We can't safely downcast Arc<dyn Trait> without extra plumbing,
    // so re-prove the second one stuck by formatting via the default
    // challenge text method (both share it) and by channel_id.
    assert_eq!(got.channel_id(), "wa");
    // Sanity: a totally different adapter does not leak in.
    assert!(reg.get("telegram").is_none());
    // Tag check via Arc::ptr identity: insert another and compare.
    let third = Arc::new(MockAdapter { id: "wa", tag: "third" });
    reg.register(third.clone());
    let got2 = reg.get("wa").expect("adapter present");
    assert!(Arc::ptr_eq(
        &got2,
        &(third as Arc<dyn PairingChannelAdapter>)
    ));
}

#[test]
fn registry_is_clone_and_shares_state() {
    let reg = PairingAdapterRegistry::new();
    let clone = reg.clone();
    reg.register(Arc::new(MockAdapter { id: "wa", tag: "x" }));
    assert!(clone.get("wa").is_some());
}

#[allow(dead_code)]
fn _silence_unused(a: &MockAdapter) -> &'static str {
    a.tag()
}
