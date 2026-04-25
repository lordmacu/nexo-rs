//! Daemon-side Unix socket server. Bins spawned by Claude (`SocketDecider`
//! in `nexo-driver-permission`) connect here and forward each
//! `permission_prompt` request to the in-process decider.

use std::path::Path;
use std::sync::Arc;

use nexo_driver_permission::{PermissionDecider, PermissionRequest, PermissionResponse};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

use crate::error::DriverError;

/// Wire frame on the line-delimited JSON socket protocol. Both sides
/// send and receive these.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SocketMessage {
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

pub struct DriverSocketServer {
    listener: UnixListener,
    decider: Arc<dyn PermissionDecider>,
    cancel: CancellationToken,
}

impl DriverSocketServer {
    /// Bind a Unix listener at `path`. If the path exists, an `unlink`
    /// is attempted first (operator may have a stale socket from a
    /// crashed daemon). File mode is set to 0600 after bind.
    pub async fn bind(
        path: &Path,
        decider: Arc<dyn PermissionDecider>,
        cancel: CancellationToken,
    ) -> Result<Self, DriverError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if path.exists() {
            // Best-effort cleanup of stale sock; ignore NotFound.
            match tokio::fs::remove_file(path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(DriverError::Socket(e.to_string())),
            }
        }
        let listener = UnixListener::bind(path).map_err(|e| DriverError::Socket(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .map_err(|e| DriverError::Socket(e.to_string()))?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms)
                .map_err(|e| DriverError::Socket(e.to_string()))?;
        }
        Ok(Self {
            listener,
            decider,
            cancel,
        })
    }

    /// Accept loop. Returns when `cancel` fires or the listener
    /// errors.
    pub async fn run(self) -> Result<(), DriverError> {
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::info!(target: "driver-socket", "shutdown signalled, draining");
                    return Ok(());
                }
                accept = self.listener.accept() => {
                    match accept {
                        Ok((stream, _addr)) => {
                            let decider = Arc::clone(&self.decider);
                            let cancel = self.cancel.clone();
                            tokio::spawn(async move {
                                if let Err(e) = serve_connection(stream, decider, cancel).await {
                                    tracing::warn!(
                                        target: "driver-socket",
                                        "connection error: {e}"
                                    );
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!(target: "driver-socket", "accept failed: {e}");
                            return Err(DriverError::Socket(e.to_string()));
                        }
                    }
                }
            }
        }
    }
}

async fn serve_connection(
    stream: UnixStream,
    decider: Arc<dyn PermissionDecider>,
    cancel: CancellationToken,
) -> Result<(), DriverError> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).lines();
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            line = reader.next_line() => line?,
        };
        let raw = match next {
            None => return Ok(()),
            Some(s) if s.trim().is_empty() => continue,
            Some(s) => s,
        };
        let msg: SocketMessage = match serde_json::from_str(&raw) {
            Ok(m) => m,
            Err(e) => {
                let err = SocketMessage::Error {
                    id: String::new(),
                    message: format!("parse error: {e}"),
                };
                write_message(&mut write_half, &err).await?;
                continue;
            }
        };
        match msg {
            SocketMessage::Decide { id, request } => {
                let response = match decider.decide(request).await {
                    Ok(r) => r,
                    Err(e) => {
                        let err = SocketMessage::Error {
                            id: id.clone(),
                            message: e.to_string(),
                        };
                        write_message(&mut write_half, &err).await?;
                        continue;
                    }
                };
                let out = SocketMessage::Decision { id, response };
                write_message(&mut write_half, &out).await?;
            }
            SocketMessage::Shutdown => return Ok(()),
            other => {
                tracing::warn!(target: "driver-socket", "unexpected message kind: {other:?}");
            }
        }
    }
}

async fn write_message(
    w: &mut (impl AsyncWriteExt + Unpin),
    msg: &SocketMessage,
) -> Result<(), DriverError> {
    let mut bytes = serde_json::to_vec(msg)?;
    bytes.push(b'\n');
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_driver_permission::{AllowAllDecider, AllowScope, PermissionOutcome, ScriptedDecider};
    use nexo_driver_types::GoalId;

    async fn write_line(w: &mut (impl AsyncWriteExt + Unpin), msg: &SocketMessage) {
        write_message(w, msg).await.unwrap();
    }

    #[tokio::test]
    async fn server_round_trip_with_allow_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("driver.sock");
        let cancel = CancellationToken::new();
        let server = DriverSocketServer::bind(&path, Arc::new(AllowAllDecider), cancel.clone())
            .await
            .unwrap();
        let handle = tokio::spawn(server.run());

        let stream = UnixStream::connect(&path).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half).lines();

        let req = PermissionRequest {
            goal_id: GoalId::new(),
            tool_use_id: "tu_x".into(),
            tool_name: "Edit".into(),
            input: serde_json::json!({"file":"x"}),
            metadata: serde_json::Map::new(),
        };
        write_line(
            &mut write_half,
            &SocketMessage::Decide {
                id: "1".into(),
                request: req,
            },
        )
        .await;

        let line = reader.next_line().await.unwrap().unwrap();
        let resp: SocketMessage = serde_json::from_str(&line).unwrap();
        match resp {
            SocketMessage::Decision { id, response } => {
                assert_eq!(id, "1");
                assert!(matches!(
                    response.outcome,
                    PermissionOutcome::AllowOnce { .. }
                ));
            }
            other => panic!("expected Decision, got {other:?}"),
        }

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn scripted_decider_iterates_across_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("driver.sock");
        let cancel = CancellationToken::new();
        let decider = Arc::new(ScriptedDecider::new([
            PermissionOutcome::AllowSession {
                scope: AllowScope::Turn,
                updated_input: None,
            },
            PermissionOutcome::Deny {
                message: "denied".into(),
            },
        ]));
        let server = DriverSocketServer::bind(&path, decider, cancel.clone())
            .await
            .unwrap();
        let handle = tokio::spawn(server.run());

        let stream = UnixStream::connect(&path).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half).lines();

        let req = || PermissionRequest {
            goal_id: GoalId::new(),
            tool_use_id: "tu".into(),
            tool_name: "Edit".into(),
            input: serde_json::json!({}),
            metadata: serde_json::Map::new(),
        };
        write_line(
            &mut write_half,
            &SocketMessage::Decide {
                id: "1".into(),
                request: req(),
            },
        )
        .await;
        let l1 = reader.next_line().await.unwrap().unwrap();
        write_line(
            &mut write_half,
            &SocketMessage::Decide {
                id: "2".into(),
                request: req(),
            },
        )
        .await;
        let l2 = reader.next_line().await.unwrap().unwrap();

        let r1: SocketMessage = serde_json::from_str(&l1).unwrap();
        let r2: SocketMessage = serde_json::from_str(&l2).unwrap();
        assert!(matches!(
            r1,
            SocketMessage::Decision {
                response: PermissionResponse {
                    outcome: PermissionOutcome::AllowSession { .. },
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            r2,
            SocketMessage::Decision {
                response: PermissionResponse {
                    outcome: PermissionOutcome::Deny { .. },
                    ..
                },
                ..
            }
        ));

        cancel.cancel();
        let _ = handle.await;
    }
}
