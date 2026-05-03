//! Generic helpers for `Microapp::with_notification_listener`
//! bridges.
//!
//! Most microapps that handle a `nexo/notify/<method>` frame
//! follow the same pattern: decode the `params: Value` into a
//! typed `T`, then fan out the typed value via a
//! `tokio::sync::broadcast::Sender<T>` so per-connection SSE
//! handlers can subscribe and filter without coupling to the
//! dispatch loop.
//!
//! [`build_broadcast_listener`] returns the `Arc<dyn Fn(Value)>`
//! shape the SDK's `with_notification_listener` expects, with
//! the decode-then-broadcast logic already wired. Lossy: a
//! malformed payload warn-logs + drops; zero-subscriber
//! `broadcast.send` errors are swallowed (the broadcast
//! channel will lazily start delivering once a subscriber
//! attaches).
//!
//! # Example
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use serde::Deserialize;
//! # use tokio::sync::broadcast;
//! # use nexo_microapp_sdk::notifications::build_broadcast_listener;
//! #[derive(Clone, Deserialize)]
//! struct PairingStatus {
//!     state: String,
//! }
//!
//! let (tx, _rx) = broadcast::channel::<PairingStatus>(64);
//! let listener: Arc<dyn Fn(serde_json::Value) + Send + Sync> =
//!     build_broadcast_listener::<PairingStatus>("pairing_status_changed", tx);
//! // Pass `listener` to `Microapp::with_notification_listener`.
//! ```

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::broadcast;

/// Build a notification listener that decodes incoming
/// `serde_json::Value` payloads into `T` and fans out the
/// typed value via the supplied `broadcast::Sender<T>`.
///
/// `method_name` is used only in the warn-log on decode
/// failure so operators can tell which notification triggered
/// the failure.
///
/// Panics: never.
pub fn build_broadcast_listener<T>(
    method_name: &'static str,
    tx: broadcast::Sender<T>,
) -> Arc<dyn Fn(Value) + Send + Sync>
where
    T: DeserializeOwned + Clone + Send + 'static,
{
    Arc::new(move |params: Value| {
        let event: T = match serde_json::from_value(params) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    method = method_name,
                    error = %e,
                    "notification listener: decode failed"
                );
                return;
            }
        };
        // `send` returns `Err` only when there are zero
        // receivers — fine, the broadcast will pick up
        // subsequent events when a subscriber attaches.
        let _ = tx.send(event);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::time::Duration;

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
    struct Pinger {
        n: u32,
    }

    #[tokio::test]
    async fn listener_decodes_and_broadcasts() {
        let (tx, mut rx) = broadcast::channel::<Pinger>(8);
        let listener = build_broadcast_listener::<Pinger>("test/ping", tx);
        listener(serde_json::json!({"n": 42}));
        let got = tokio::time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("recv timeout")
            .expect("recv ok");
        assert_eq!(got, Pinger { n: 42 });
    }

    #[tokio::test]
    async fn listener_drops_malformed_payload_without_panic() {
        let (tx, mut rx) = broadcast::channel::<Pinger>(8);
        let listener = build_broadcast_listener::<Pinger>("test/ping", tx);
        // Garbage payload — listener must NOT panic, NOT
        // broadcast.
        listener(serde_json::json!({"not": "a real ping"}));
        let res = tokio::time::timeout(Duration::from_millis(20), rx.recv()).await;
        assert!(res.is_err(), "no event should be broadcast on malformed input");
    }

    #[tokio::test]
    async fn listener_continues_when_no_subscribers() {
        let (tx, _rx) = broadcast::channel::<Pinger>(8);
        // Drop the only receiver so `send` returns Err.
        drop(_rx);
        let listener = build_broadcast_listener::<Pinger>("test/ping", tx);
        // Surviving the call without panic is the contract.
        listener(serde_json::json!({"n": 7}));
    }
}
