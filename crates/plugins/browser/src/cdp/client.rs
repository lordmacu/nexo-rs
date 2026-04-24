use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

struct CdpRequest {
    id: u64,
    method: String,
    params: Value,
    session_id: Option<String>,
}

/// A CDP notification (no `id` field) received from Chrome.
/// Forwarded via a broadcast channel so multiple subscribers can observe it.
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

type PendingMap = Arc<DashMap<u64, oneshot::Sender<anyhow::Result<Value>>>>;

#[derive(Clone)]
pub struct CdpClient {
    tx: mpsc::Sender<CdpRequest>,
    pending: PendingMap,
    events: broadcast::Sender<CdpEvent>,
    next_id: Arc<AtomicU64>,
    shutdown: CancellationToken,
}

impl CdpClient {
    /// Connect to a raw CDP WebSocket URL (ws://...).
    pub async fn connect(ws_url: &str) -> anyhow::Result<Self> {
        let (ws_stream, _) = connect_async(ws_url)
            .await
            .map_err(|e| anyhow!("CDP WebSocket connect failed: {e}"))?;

        let (mut sink, mut stream) = ws_stream.split();
        let (tx, mut rx) = mpsc::channel::<CdpRequest>(64);
        let pending: PendingMap = Arc::new(DashMap::new());
        let (events_tx, _) = broadcast::channel::<CdpEvent>(256);
        let shutdown = CancellationToken::new();

        // Writer task
        let pending_w = Arc::clone(&pending);
        let shutdown_w = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    req = rx.recv() => {
                        let Some(req) = req else { break };
                        let mut msg = json!({
                            "id": req.id,
                            "method": req.method,
                            "params": req.params,
                        });
                        if let Some(sid) = req.session_id {
                            msg["sessionId"] = json!(sid);
                        }
                        if sink.send(Message::Text(msg.to_string())).await.is_err() {
                            // Socket is gone — fail every caller waiting
                            // on a response so they unblock immediately
                            // rather than sitting until their timeout.
                            // (The earlier iter/retain no-ops here were
                            // unused drafts — removed.)
                            let ids: Vec<u64> = pending_w.iter().map(|e| *e.key()).collect();
                            for id in ids {
                                if let Some((_, sender)) = pending_w.remove(&id) {
                                    let _ = sender.send(Err(anyhow!("CDP connection lost")));
                                }
                            }
                            break;
                        }
                    }
                    _ = shutdown_w.cancelled() => {
                        let _ = sink.close().await;
                        break;
                    }
                }
            }
        });

        // Reader task
        let pending_r = Arc::clone(&pending);
        let events_r = events_tx.clone();
        let shutdown_r = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = stream.next() => {
                        let Some(Ok(msg)) = msg else {
                            // Reject all pending on disconnect
                            let ids: Vec<u64> = pending_r.iter().map(|e| *e.key()).collect();
                            for id in ids {
                                if let Some((_, s)) = pending_r.remove(&id) {
                                    let _ = s.send(Err(anyhow!("CDP socket closed")));
                                }
                            }
                            break;
                        };
                        let text = match msg {
                            Message::Text(t) => t.to_string(),
                            Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                            Message::Close(_) => {
                                let ids: Vec<u64> = pending_r.iter().map(|e| *e.key()).collect();
                                for id in ids {
                                    if let Some((_, s)) = pending_r.remove(&id) {
                                        let _ = s.send(Err(anyhow!("CDP socket closed")));
                                    }
                                }
                                break;
                            }
                            _ => continue,
                        };
                        let Ok(parsed) = serde_json::from_str::<Value>(&text) else { continue };

                        // Messages with an `id` are responses to requests.
                        if let Some(id) = parsed.get("id").and_then(|v| v.as_u64()) {
                            let Some((_, sender)) = pending_r.remove(&id) else { continue };
                            if let Some(err) = parsed.get("error") {
                                let msg = err.get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("CDP error");
                                let _ = sender.send(Err(anyhow!("{msg}")));
                            } else {
                                let result = parsed.get("result").cloned().unwrap_or(Value::Null);
                                let _ = sender.send(Ok(result));
                            }
                            continue;
                        }

                        // Otherwise it's an event notification (no `id`, has `method`).
                        if let Some(method) = parsed.get("method").and_then(|v| v.as_str()) {
                            let params = parsed.get("params").cloned().unwrap_or(Value::Null);
                            let session_id = parsed
                                .get("sessionId")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            // Ignore send errors — no subscribers is fine.
                            let _ = events_r.send(CdpEvent {
                                method: method.to_string(),
                                params,
                                session_id,
                            });
                        }
                    }
                    _ = shutdown_r.cancelled() => break,
                }
            }
        });

        Ok(Self {
            tx,
            pending,
            events: events_tx,
            next_id: Arc::new(AtomicU64::new(1)),
            shutdown,
        })
    }

    /// Subscribe to every CDP event. Caller filters by `method` / `session_id`.
    pub fn subscribe_events(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    /// Subscribe to events scoped to a specific `session_id`. Events for other
    /// sessions are discarded by a filter task to keep the receiver simple.
    pub fn subscribe_session_events(&self, session_id: &str) -> broadcast::Receiver<CdpEvent> {
        let session_id = session_id.to_string();
        let mut upstream = self.events.subscribe();
        let (downstream_tx, downstream_rx) = broadcast::channel::<CdpEvent>(64);
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Respect client shutdown so these filter tasks
                    // don't outlive the CdpClient and leak into the
                    // runtime when many sessions come and go.
                    _ = shutdown.cancelled() => break,
                    recv = upstream.recv() => match recv {
                        Ok(ev) if ev.session_id.as_deref() == Some(&session_id) => {
                            if downstream_tx.send(ev).is_err() {
                                break; // no subscribers left
                            }
                        }
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        });
        downstream_rx
    }

    /// Discover WS URL from a CDP HTTP endpoint (http://127.0.0.1:9222).
    pub async fn discover_ws_url(http_url: &str, timeout_ms: u64) -> anyhow::Result<String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()?;
        let url = format!("{}/json/version", http_url.trim_end_matches('/'));
        // Chrome DevTools HTTP endpoint requires Host: localhost.
        let resp: Value = client
            .get(&url)
            .header(reqwest::header::HOST, "localhost")
            .send()
            .await?
            .json()
            .await?;
        let ws = resp
            .get("webSocketDebuggerUrl")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no webSocketDebuggerUrl in /json/version response"))?;
        // Chrome echoes the Host header back in webSocketDebuggerUrl (e.g. ws://localhost/...),
        // which loses the real port. Rewrite authority to match the cdp_url we were given.
        Ok(rewrite_ws_authority(ws, http_url))
    }

    pub async fn send(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.send_with_session(method, params, None).await
    }

    /// Default per-call timeout. Covers most CDP roundtrips (navigate,
    /// querySelector, eval). Long-running operations should use
    /// `send_with_timeout` explicitly.
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    pub async fn send_with_session(
        &self,
        method: &str,
        params: Value,
        session_id: Option<String>,
    ) -> anyhow::Result<Value> {
        self.send_with_timeout(method, params, session_id, Self::DEFAULT_TIMEOUT)
            .await
    }

    /// Like `send_with_session` but with a caller-specified timeout.
    /// Without an upper bound a CDP request waiting on a crashed tab
    /// would hang forever — every other client in this workspace caps
    /// its waits, so browser gets the same treatment.
    pub async fn send_with_timeout(
        &self,
        method: &str,
        params: Value,
        session_id: Option<String>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        if self
            .tx
            .send(CdpRequest {
                id,
                method: method.to_string(),
                params,
                session_id,
            })
            .await
            .is_err()
        {
            // Writer gone — reclaim the slot so a late drain doesn't
            // double-fire, then surface a clean error.
            self.pending.remove(&id);
            return Err(anyhow!("CDP client channel closed"));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("CDP response channel dropped")),
            Err(_) => {
                // Timed out — remove the pending slot so a late
                // response doesn't find (and try to use) a stale
                // sender that's already dropped. Caller sees a clean
                // timeout error identical in shape to the other
                // clients in the workspace.
                self.pending.remove(&id);
                Err(anyhow!(
                    "CDP request `{method}` timed out after {}ms",
                    timeout.as_millis()
                ))
            }
        }
    }

    pub fn close(&self) {
        self.shutdown.cancel();
    }
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

// Re-export reqwest for use inside the crate
use reqwest;

/// Replace the authority (host:port) of a ws:// or wss:// URL with the authority
/// from an http:// or https:// base. Preserves scheme and path of the WS URL.
/// Chrome returns webSocketDebuggerUrl echoing the Host header, which drops the
/// real port when Host was spoofed — this rewrites it back to the reachable endpoint.
fn rewrite_ws_authority(ws_url: &str, http_base: &str) -> String {
    let authority = http_base
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let (ws_scheme, rest) = if let Some(r) = ws_url.strip_prefix("ws://") {
        ("ws://", r)
    } else if let Some(r) = ws_url.strip_prefix("wss://") {
        ("wss://", r)
    } else {
        return ws_url.to_string();
    };
    let path = rest.find('/').map(|i| &rest[i..]).unwrap_or("");
    format!("{ws_scheme}{authority}{path}")
}

#[cfg(test)]
mod tests {
    use super::rewrite_ws_authority;

    #[test]
    fn rewrites_host_and_port() {
        let out = rewrite_ws_authority("ws://localhost/devtools/browser/abc", "http://chrome:9222");
        assert_eq!(out, "ws://chrome:9222/devtools/browser/abc");
    }

    #[test]
    fn preserves_existing_port() {
        let out = rewrite_ws_authority(
            "ws://localhost:1234/devtools/browser/xyz",
            "http://127.0.0.1:9222/",
        );
        assert_eq!(out, "ws://127.0.0.1:9222/devtools/browser/xyz");
    }

    #[test]
    fn handles_wss() {
        let out = rewrite_ws_authority(
            "wss://localhost/devtools/page/1",
            "https://remote.example:443",
        );
        assert_eq!(out, "wss://remote.example:443/devtools/page/1");
    }

    #[tokio::test]
    async fn send_with_timeout_reports_timeout_cleanly() {
        // Drive the client without a real Chrome: stub a writer task
        // that silently eats requests and never replies. `send_with_
        // timeout` must return an Err that mentions "timed out" (and
        // not hang the test runner).
        use super::CdpClient;
        use dashmap::DashMap;
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc as StdArc;
        use tokio::sync::{broadcast, mpsc};
        use tokio_util::sync::CancellationToken;

        let (tx, mut rx) = mpsc::channel::<super::CdpRequest>(8);
        let (events_tx, _) = broadcast::channel(4);
        let shutdown = CancellationToken::new();
        let shutdown_c = shutdown.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown_c.cancelled() => {}
                _ = async { while let Some(_req) = rx.recv().await {} } => {}
            }
        });
        // Reconstruct a private-like CdpClient via the same fields.
        // We can't use CdpClient::connect without a server, so the
        // struct has to be buildable here. It is — all fields are
        // internal and this test lives in the same module.
        let client = CdpClient {
            tx,
            pending: StdArc::new(DashMap::new()),
            events: events_tx,
            next_id: StdArc::new(AtomicU64::new(1)),
            shutdown,
        };
        let err = client
            .send_with_timeout(
                "Test.hang",
                serde_json::json!({}),
                None,
                std::time::Duration::from_millis(50),
            )
            .await
            .unwrap_err();
        let s = err.to_string();
        assert!(
            s.contains("timed out"),
            "error must mention timeout; got: {s}"
        );
        // Pending entry must be removed — a late response arriving now
        // shouldn't find a sender waiting, otherwise we'd leak.
        assert!(client.pending.is_empty(), "pending slot must be reclaimed");
    }
}
