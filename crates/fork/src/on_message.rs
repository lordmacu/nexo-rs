//! `OnMessage` — observation hook over the fork's message stream.
//! Step 80.19 / 7 (executed alongside steps 4-6 — interdependency).
//!
//! Mirrors KAIROS's `onMessage?: (message: Message) => void` callback
//! (leak `forkedAgent.ts:106`). Consumers like Phase 80.1's
//! `FilesTouchedCollector` build collectors that scan tool_use blocks
//! for `FileEdit` / `FileWrite` paths.
//!
//! Panic safety: [`ChainCollector`] catches panics in inner collectors
//! so a buggy hook cannot kill the fork's turn loop.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::FutureExt;
use nexo_llm::types::ChatMessage;
use tracing::warn;

#[async_trait]
pub trait OnMessage: Send + Sync {
    async fn on_message(&self, msg: &ChatMessage);
}

/// Cheap default — does nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopCollector;

#[async_trait]
impl OnMessage for NoopCollector {
    async fn on_message(&self, _msg: &ChatMessage) {}
}

/// Logs each message at `tracing::debug!` for development.
#[derive(Debug, Clone)]
pub struct LoggingCollector {
    pub label: String,
}

impl LoggingCollector {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

#[async_trait]
impl OnMessage for LoggingCollector {
    async fn on_message(&self, msg: &ChatMessage) {
        tracing::debug!(
            target: "fork.on_message",
            label = %self.label,
            role = ?msg.role,
            tool_calls = msg.tool_calls.len(),
            "fork message"
        );
    }
}

/// Fan-out to N inner collectors. Each call is wrapped in
/// `catch_unwind` so a panicking collector cannot kill the loop.
pub struct ChainCollector {
    inner: Vec<Box<dyn OnMessage>>,
}

impl ChainCollector {
    pub fn new(inner: Vec<Box<dyn OnMessage>>) -> Self {
        Self { inner }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[async_trait]
impl OnMessage for ChainCollector {
    async fn on_message(&self, msg: &ChatMessage) {
        for (idx, c) in self.inner.iter().enumerate() {
            let result = AssertUnwindSafe(c.on_message(msg)).catch_unwind().await;
            if let Err(e) = result {
                let panic_msg = e
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| e.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown".into());
                warn!(
                    target: "fork.on_message",
                    collector_index = idx,
                    panic = %panic_msg,
                    "collector panicked — continuing chain"
                );
            }
        }
    }
}

/// Test helper — counts each invocation atomically.
#[derive(Debug, Default)]
pub struct CountingCollector {
    pub count: AtomicUsize,
}

#[async_trait]
impl OnMessage for CountingCollector {
    async fn on_message(&self, _msg: &ChatMessage) {
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn dummy_msg() -> ChatMessage {
        ChatMessage::user("hello")
    }

    #[tokio::test]
    async fn noop_collector_does_nothing() {
        let c = NoopCollector;
        c.on_message(&dummy_msg()).await;
    }

    #[tokio::test]
    async fn chain_invokes_all_inner_collectors() {
        let a = Arc::new(CountingCollector::default());
        let b = Arc::new(CountingCollector::default());
        struct Forward(Arc<CountingCollector>);
        #[async_trait]
        impl OnMessage for Forward {
            async fn on_message(&self, m: &ChatMessage) {
                self.0.on_message(m).await;
            }
        }
        let chain = ChainCollector::new(vec![
            Box::new(Forward(a.clone())),
            Box::new(Forward(b.clone())),
        ]);
        chain.on_message(&dummy_msg()).await;
        chain.on_message(&dummy_msg()).await;
        assert_eq!(a.count.load(Ordering::Relaxed), 2);
        assert_eq!(b.count.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn chain_swallows_panic_from_inner_collector() {
        struct Panicking;
        #[async_trait]
        impl OnMessage for Panicking {
            async fn on_message(&self, _m: &ChatMessage) {
                panic!("boom");
            }
        }
        let counter = Arc::new(CountingCollector::default());
        struct Forward(Arc<CountingCollector>);
        #[async_trait]
        impl OnMessage for Forward {
            async fn on_message(&self, m: &ChatMessage) {
                self.0.on_message(m).await;
            }
        }
        let chain = ChainCollector::new(vec![
            Box::new(Panicking),
            Box::new(Forward(counter.clone())),
        ]);
        // Must NOT panic; later collectors still fire.
        chain.on_message(&dummy_msg()).await;
        assert_eq!(counter.count.load(Ordering::Relaxed), 1);
    }
}
