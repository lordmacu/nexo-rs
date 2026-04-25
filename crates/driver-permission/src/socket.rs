//! Bin-side `SocketDecider`: a `PermissionDecider` that forwards each
//! request over a Unix socket to the daemon. Lives in `nexo-driver-
//! permission` (not `nexo-driver-loop`) to avoid a dep cycle — the
//! permission bin needs this client without pulling in the loop crate.
//!
//! Wire protocol mirrors `nexo_driver_loop::socket::SocketMessage`:
//! line-delimited JSON, tagged enum.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::decider::PermissionDecider;
use crate::error::PermissionError;
use crate::types::{PermissionOutcome, PermissionRequest, PermissionResponse};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SocketMessage {
    Decide {
        id: String,
        request: PermissionRequest,
    },
    Decision {
        id: String,
        response: PermissionResponse,
    },
    Error {
        id: String,
        message: String,
    },
    Shutdown,
}

pub struct SocketDecider {
    socket_path: PathBuf,
    timeout: Duration,
    counter: AtomicU64,
    inner: Mutex<Option<Connection>>,
}

struct Connection {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl SocketDecider {
    pub fn new(socket_path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout,
            counter: AtomicU64::new(0),
            inner: Mutex::new(None),
        }
    }

    fn next_id(&self) -> String {
        self.counter.fetch_add(1, Ordering::Relaxed).to_string()
    }

    async fn connect(&self) -> Result<Connection, PermissionError> {
        let stream = UnixStream::connect(&self.socket_path).await.map_err(|e| {
            PermissionError::Decider(format!("connect {:?}: {e}", self.socket_path))
        })?;
        let (read_half, write_half) = stream.into_split();
        Ok(Connection {
            reader: BufReader::new(read_half),
            writer: write_half,
        })
    }
}

fn unavailable(req_id: String, reason: String) -> PermissionResponse {
    PermissionResponse {
        tool_use_id: req_id,
        outcome: PermissionOutcome::Unavailable { reason },
        rationale: String::new(),
    }
}

#[async_trait]
impl PermissionDecider for SocketDecider {
    async fn decide(
        &self,
        request: PermissionRequest,
    ) -> Result<PermissionResponse, PermissionError> {
        let tool_use_id = request.tool_use_id.clone();
        let id = self.next_id();
        let frame = SocketMessage::Decide {
            id: id.clone(),
            request,
        };
        let mut bytes = match serde_json::to_vec(&frame) {
            Ok(b) => b,
            Err(e) => {
                return Ok(unavailable(tool_use_id, format!("encode request: {e}")));
            }
        };
        bytes.push(b'\n');

        let result = tokio::time::timeout(self.timeout, async {
            let mut guard = self.inner.lock().await;
            if guard.is_none() {
                *guard = Some(self.connect().await?);
            }

            // Try once; on transport failure reconnect and retry.
            for attempt in 0..2 {
                let conn = guard.as_mut().unwrap();
                if conn.writer.write_all(&bytes).await.is_err()
                    || conn.writer.flush().await.is_err()
                {
                    *guard = Some(self.connect().await?);
                    if attempt == 1 {
                        return Err(PermissionError::Decider("socket write failed twice".into()));
                    }
                    continue;
                }
                let mut line = String::new();
                let n: usize = conn.reader.read_line(&mut line).await.unwrap_or_default();
                if n == 0 {
                    *guard = Some(self.connect().await?);
                    if attempt == 1 {
                        return Err(PermissionError::Decider("socket closed by peer".into()));
                    }
                    continue;
                }
                let msg: SocketMessage = serde_json::from_str(line.trim())
                    .map_err(|e| PermissionError::Decider(format!("decode response: {e}")))?;
                return match msg {
                    SocketMessage::Decision { response, .. } => Ok(response),
                    SocketMessage::Error { message, .. } => Err(PermissionError::Decider(message)),
                    other => Err(PermissionError::Decider(format!(
                        "unexpected frame: {other:?}"
                    ))),
                };
            }
            Err(PermissionError::Decider("socket retry exhausted".into()))
        })
        .await;

        match result {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => Ok(unavailable(tool_use_id, e.to_string())),
            Err(_) => Ok(unavailable(tool_use_id, "socket timeout".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decider::AllowAllDecider;
    use serde_json::json;
    use tokio::net::UnixListener;
    use tokio_util::sync::CancellationToken;

    /// Tiny mock server matching the daemon-side protocol; uses the
    /// supplied decider closure-style so tests can wire deny/allow.
    async fn run_mock_server(
        listener: UnixListener,
        decider: std::sync::Arc<dyn PermissionDecider>,
        cancel: CancellationToken,
    ) {
        loop {
            let stream = tokio::select! {
                _ = cancel.cancelled() => return,
                a = listener.accept() => match a {
                    Ok((s, _)) => s,
                    Err(_) => return,
                },
            };
            let decider = std::sync::Arc::clone(&decider);
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    if cancel.is_cancelled() {
                        return;
                    }
                    if line.trim().is_empty() {
                        continue;
                    }
                    let msg: SocketMessage = match serde_json::from_str(&line) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if let SocketMessage::Decide { id, request } = msg {
                        let resp = decider.decide(request).await.unwrap();
                        let out = SocketMessage::Decision { id, response: resp };
                        let mut bytes = serde_json::to_vec(&out).unwrap();
                        bytes.push(b'\n');
                        let _ = write_half.write_all(&bytes).await;
                        let _ = write_half.flush().await;
                    }
                }
            });
        }
    }

    #[tokio::test]
    async fn round_trip_against_mock_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("driver.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let cancel = CancellationToken::new();
        let server_decider: std::sync::Arc<dyn PermissionDecider> =
            std::sync::Arc::new(AllowAllDecider);
        let handle = tokio::spawn(run_mock_server(listener, server_decider, cancel.clone()));

        let client = SocketDecider::new(path, Duration::from_secs(2));
        let req = PermissionRequest {
            goal_id: nexo_driver_types::GoalId::new(),
            tool_use_id: "tu_x".into(),
            tool_name: "Edit".into(),
            input: json!({"a": 1}),
            metadata: serde_json::Map::new(),
        };
        let resp = client.decide(req).await.unwrap();
        assert!(matches!(resp.outcome, PermissionOutcome::AllowOnce { .. }));

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn unreachable_socket_returns_unavailable_outcome() {
        let path = std::path::PathBuf::from("/tmp/nexo-driver-not-a-real-socket-XYZ");
        let _ = std::fs::remove_file(&path);
        let client = SocketDecider::new(path, Duration::from_millis(200));
        let req = PermissionRequest {
            goal_id: nexo_driver_types::GoalId::new(),
            tool_use_id: "tu_x".into(),
            tool_name: "Edit".into(),
            input: json!({}),
            metadata: serde_json::Map::new(),
        };
        let resp = client.decide(req).await.unwrap();
        assert!(matches!(
            resp.outcome,
            PermissionOutcome::Unavailable { .. }
        ));
    }
}
