//! Filtered SSE stream over a `tokio::broadcast::Receiver<T>`.
//!
//! Every microapp that fans out daemon notifications via Server-
//! Sent Events lands the same skeleton:
//!
//! 1. Subscribe to the in-memory `broadcast::Sender<T>`.
//! 2. Loop `recv().await`, filter by per-connection query
//!    parameters, serialise the typed value to JSON, yield as an
//!    `Event::default().data(...)`.
//! 3. Optionally close the stream when a terminal value arrives
//!    (e.g. pairing flow stops on `Linked`/`Expired`/`Cancelled`).
//! 4. Decide what to do on `RecvError::Lagged` — skip, or surface
//!    the gap via a `lagged` event the client uses to reconcile.
//!
//! [`sse_filtered_broadcast`] takes the variability as
//! parameters (filter, terminate, lagged behavior, event name)
//! and returns the `Sse<...>` wrapper ready to hand back from an
//! axum handler. Caller writes one line per route instead of
//! hand-rolling the `async_stream::stream! { … }` block.
//!
//! # Example
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use serde::Serialize;
//! # use tokio::sync::broadcast;
//! # use nexo_microapp_http::sse::{sse_filtered_broadcast, LaggedBehavior};
//! # #[derive(Clone, Serialize)] struct Pairing { state: String }
//! # async fn handler(rx: broadcast::Receiver<Pairing>) {
//! let target = "abc".to_string();
//! let _sse = sse_filtered_broadcast::<Pairing, _, _>(
//!     rx,
//!     None,
//!     move |p: &Pairing| p.state.starts_with(&target),
//!     |p: &Pairing| matches!(p.state.as_str(), "linked" | "expired"),
//!     LaggedBehavior::Skip,
//! );
//! # }
//! ```

use std::convert::Infallible;
use std::time::Duration;

use async_stream::stream;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

/// What to do when the broadcast subscriber falls behind the
/// channel buffer (`RecvError::Lagged(n)`).
#[derive(Debug, Clone)]
pub enum LaggedBehavior {
    /// Silently skip the gap. The next `recv()` returns whatever
    /// is now at the head of the channel. Suitable when
    /// downstream clients have a polling fallback.
    Skip,
    /// Yield an `event: lagged` SSE frame carrying a JSON body
    /// `{"dropped": <n>}` so the client knows to reconcile via
    /// backfill / polling. The `event_name` is the SSE field
    /// name (typically `"lagged"`).
    Emit {
        /// SSE `event:` field for the synthetic lagged frame.
        event_name: &'static str,
    },
}

/// Default keep-alive interval. Most browsers close idle SSE
/// connections after ~30 s; 15 s leaves a safe margin.
pub const DEFAULT_KEEP_ALIVE_SECS: u64 = 15;

/// Build a filtered SSE stream over a `broadcast::Receiver<T>`.
///
/// Each `T` that survives `filter` is JSON-serialised and yielded
/// as an `Event::default().data(...)`. When `event_name` is
/// `Some`, `Event::default().event(name)` tags every frame; the
/// SSE client then dispatches with `addEventListener("name", …)`.
///
/// `terminate` is called on each yielded value AFTER it leaves
/// the stream; returning `true` closes the connection. Useful for
/// finite flows (pairing's terminal states); always-on streams
/// pass `|_| false`.
///
/// `RecvError::Closed` always closes the stream. `Lagged(n)` is
/// handled per [`LaggedBehavior`].
///
/// Serialisation failures (extremely unlikely for `Serialize` +
/// derive-based types) are logged via `tracing::warn` and the
/// frame is dropped silently — closing the stream on a single
/// bad value would punish the rest of the broadcast.
pub fn sse_filtered_broadcast<T, F, G>(
    mut rx: broadcast::Receiver<T>,
    event_name: Option<&'static str>,
    filter: F,
    terminate: G,
    on_lagged: LaggedBehavior,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    T: Serialize + Clone + Send + 'static,
    F: Fn(&T) -> bool + Send + Sync + 'static,
    G: Fn(&T) -> bool + Send + Sync + 'static,
{
    let s = stream! {
        loop {
            match rx.recv().await {
                Ok(value) => {
                    if !filter(&value) {
                        continue;
                    }
                    let payload = match serde_json::to_string(&value) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(error = %e, "sse_filtered_broadcast: serialize failed; dropping frame");
                            continue;
                        }
                    };
                    let event = match event_name {
                        Some(name) => Event::default().event(name).data(payload),
                        None => Event::default().data(payload),
                    };
                    yield Ok(event);
                    if terminate(&value) {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
                Err(RecvError::Lagged(n)) => match &on_lagged {
                    LaggedBehavior::Skip => continue,
                    LaggedBehavior::Emit { event_name } => {
                        yield Ok(Event::default()
                            .event(*event_name)
                            .data(format!(r#"{{"dropped":{n}}}"#)));
                        continue;
                    }
                },
            }
        }
    };
    Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(DEFAULT_KEEP_ALIVE_SECS)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use tower::ServiceExt;

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
    struct Tick {
        id: u32,
        terminal: bool,
    }

    /// Drain the SSE response body and split into individual
    /// frames separated by blank lines. Each entry returned is
    /// the raw frame text including `event:` / `data:` lines.
    async fn drain_frames(res: axum::response::Response) -> Vec<String> {
        let bytes = BodyExt::collect(res.into_body())
            .await
            .unwrap()
            .to_bytes();
        let body = String::from_utf8_lossy(&bytes).to_string();
        body.split("\n\n")
            .filter(|s| !s.trim().is_empty() && !s.trim().starts_with(":"))
            .map(|s| s.to_string())
            .collect()
    }

    fn frame_data(frame: &str) -> Option<&str> {
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("data: ") {
                return Some(rest);
            }
        }
        None
    }

    fn frame_event_name(frame: &str) -> Option<&str> {
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("event: ") {
                return Some(rest);
            }
        }
        None
    }

    #[tokio::test]
    async fn yields_filtered_frames_with_event_name() {
        let (tx, _rx) = broadcast::channel::<Tick>(16);
        let tx_arc = Arc::new(tx);
        let tx_for_route = Arc::clone(&tx_arc);
        let app: Router = Router::new().route(
            "/stream",
            get(move || {
                let tx = Arc::clone(&tx_for_route);
                async move {
                    sse_filtered_broadcast(
                        tx.subscribe(),
                        Some("tick"),
                        |t: &Tick| t.id % 2 == 0,
                        |t: &Tick| t.terminal,
                        LaggedBehavior::Skip,
                    )
                    .into_response()
                }
            }),
        );

        // Pre-publish so the events are sitting in the buffer
        // when the subscriber attaches via the request.
        let publisher = Arc::clone(&tx_arc);
        tokio::spawn(async move {
            // Small delay so the request has time to subscribe.
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = publisher.send(Tick { id: 1, terminal: false }); // filtered out
            let _ = publisher.send(Tick { id: 2, terminal: false }); // pass
            let _ = publisher.send(Tick { id: 4, terminal: true }); // pass + terminate
        });

        let res = app
            .oneshot(Request::builder().uri("/stream").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let frames = drain_frames(res).await;
        assert_eq!(frames.len(), 2, "expected 2 frames, got {:?}", frames);
        assert_eq!(frame_event_name(&frames[0]), Some("tick"));
        let t0: Tick = serde_json::from_str(frame_data(&frames[0]).unwrap()).unwrap();
        assert_eq!(t0.id, 2);
        let t1: Tick = serde_json::from_str(frame_data(&frames[1]).unwrap()).unwrap();
        assert_eq!(t1.id, 4);
    }

    #[tokio::test]
    async fn lagged_emit_yields_lagged_frame() {
        // Capacity 2 so a slow subscriber goes Lagged after a
        // few publishes.
        let (tx, _rx) = broadcast::channel::<Tick>(2);
        let tx_arc = Arc::new(tx);
        let tx_for_route = Arc::clone(&tx_arc);
        let app: Router = Router::new().route(
            "/stream",
            get(move || {
                let tx = Arc::clone(&tx_for_route);
                async move {
                    sse_filtered_broadcast(
                        tx.subscribe(),
                        None,
                        |_: &Tick| true,
                        |t: &Tick| t.terminal,
                        LaggedBehavior::Emit {
                            event_name: "lagged",
                        },
                    )
                    .into_response()
                }
            }),
        );

        let publisher = Arc::clone(&tx_arc);
        tokio::spawn(async move {
            // Force the subscriber to fall behind: publish more
            // than `capacity` while the request handler is still
            // attaching.
            tokio::time::sleep(Duration::from_millis(20)).await;
            for i in 0..6u32 {
                let _ = publisher.send(Tick { id: i, terminal: false });
            }
            let _ = publisher.send(Tick { id: 99, terminal: true });
        });

        let res = app
            .oneshot(Request::builder().uri("/stream").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let frames = drain_frames(res).await;
        // We expect at least one `lagged` frame followed by the
        // terminal `id: 99` frame.
        assert!(
            frames.iter().any(|f| frame_event_name(f) == Some("lagged")),
            "expected a `lagged` frame, got {:?}",
            frames
        );
        let last = frames.last().unwrap();
        let t: Tick = serde_json::from_str(frame_data(last).unwrap()).unwrap();
        assert_eq!(t.id, 99);
    }

    #[tokio::test]
    async fn closed_channel_ends_stream() {
        let (tx, _rx) = broadcast::channel::<Tick>(8);
        // Drop tx after the route subscribes — receiver sees Closed.
        let tx_arc = Arc::new(tx);
        let tx_for_route = Arc::clone(&tx_arc);
        let app: Router = Router::new().route(
            "/stream",
            get(move || {
                let tx = Arc::clone(&tx_for_route);
                async move {
                    sse_filtered_broadcast(
                        tx.subscribe(),
                        None,
                        |_: &Tick| true,
                        |_: &Tick| false,
                        LaggedBehavior::Skip,
                    )
                    .into_response()
                }
            }),
        );

        // Ensure the route subscribes BEFORE we drop the sender.
        let publisher = Arc::clone(&tx_arc);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            // Drop the last sender clone the test holds — together
            // with the route's `tx_for_route` clone we still have
            // 1 alive. Drop both to close.
            drop(publisher);
        });
        // Drop the outer test-side reference — route still holds
        // its own clone, so this alone won't close.
        drop(tx_arc);

        let res = app
            .oneshot(Request::builder().uri("/stream").body(Body::empty()).unwrap())
            .await
            .unwrap();
        // No frames produced; the body simply ends. We just check
        // the request completed (a hung stream would block here).
        let _ = drain_frames(res).await;
    }
}
