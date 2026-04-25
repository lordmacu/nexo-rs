//! Lookup table mapping `channel_id` → adapter implementation.
//!
//! The runtime owns one of these and consults it on the inbound hot
//! path: when the gate issues a `Decision::Challenge`, the runtime
//! calls `registry.get(channel)` to format and deliver the code via
//! the channel-specific adapter. Channels with no registered adapter
//! fall back to a hardcoded broker publish that mirrors the legacy
//! shape (`{kind:"text", to, text}` on `plugin.outbound.{channel}`).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::adapter::PairingChannelAdapter;

#[derive(Clone, Default)]
pub struct PairingAdapterRegistry {
    inner: Arc<RwLock<HashMap<&'static str, Arc<dyn PairingChannelAdapter>>>>,
}

impl PairingAdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or overwrite) the adapter for its declared `channel_id`.
    pub fn register(&self, adapter: Arc<dyn PairingChannelAdapter>) {
        let id = adapter.channel_id();
        self.inner.write().unwrap().insert(id, adapter);
    }

    /// Lookup. Returns `None` for unregistered channels — callers
    /// should fall back to legacy broker publish behaviour.
    pub fn get(&self, channel_id: &str) -> Option<Arc<dyn PairingChannelAdapter>> {
        self.inner.read().unwrap().get(channel_id).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().is_empty()
    }
}
