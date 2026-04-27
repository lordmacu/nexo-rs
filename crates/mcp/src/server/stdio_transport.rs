//! Phase 76.2 — newline-delimited JSON `McpTransport` over arbitrary
//! `AsyncRead`/`AsyncWrite` halves. Used by:
//!   * `super::stdio::run_with_io_auth` (real `tokio::io::stdin/stdout`)
//!   * the inline integration tests in `super::stdio` and the
//!     `tests/server_test.rs` fixture (both feed `tokio::io::duplex`).
//!
//! Framing rules (preserved byte-for-byte from Phase 12.6):
//!   * One JSON object per line, terminated by `\n`.
//!   * Empty / whitespace-only lines are skipped (NOT errors).
//!   * Invalid JSON is surfaced as `TransportError::InvalidFrame`;
//!     the driver loop logs and continues, matching pre-refactor
//!     behaviour.
//!   * Stdin EOF surfaces as `TransportError::Closed`; the driver
//!     terminates the loop normally.

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use super::dispatch::{DispatchContext, DispatchOutcome, Dispatcher};
use super::transport::{Frame, McpTransport, TransportError};
use super::McpServerHandler;

/// Newline-delimited JSON transport. Generic over the read/write
/// halves so the same impl powers both real stdio and `duplex`-fed
/// tests.
pub(crate) struct StdioTransport<I, O> {
    reader: BufReader<I>,
    writer: O,
    closed: bool,
}

impl<I, O> StdioTransport<I, O>
where
    I: AsyncRead + Unpin + Send,
    O: AsyncWrite + Unpin + Send,
{
    pub(crate) fn new(stdin: I, stdout: O) -> Self {
        Self {
            reader: BufReader::new(stdin),
            writer: stdout,
            closed: false,
        }
    }
}

#[async_trait]
impl<I, O> McpTransport for StdioTransport<I, O>
where
    I: AsyncRead + Unpin + Send,
    O: AsyncWrite + Unpin + Send,
{
    async fn read_frame(&mut self) -> Result<Frame, TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.reader.read_line(&mut line).await?;
            if n == 0 {
                self.closed = true;
                return Err(TransportError::Closed);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return match serde_json::from_str::<Value>(trimmed) {
                Ok(v) => Ok(Frame::new(v)),
                Err(e) => Err(TransportError::InvalidFrame(format!("{e}: raw={trimmed}"))),
            };
        }
    }

    async fn write_frame(&mut self, frame: Frame) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        let mut body = frame.payload.to_string();
        body.push('\n');
        self.writer.write_all(body.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        if !self.closed {
            self.closed = true;
            let _ = self.writer.flush().await;
        }
        Ok(())
    }
}

/// Generic driver loop: read frames from the transport, dispatch
/// against the supplied `Dispatcher`, write responses back.
/// Sequential per-connection (one inbound request at a time), which
/// matches both the pre-refactor stdio behaviour and the
/// per-session contract that HTTP (76.1) will inherit.
///
/// Termination conditions, in order of precedence:
///   1. `shutdown` cancelled → break, close transport, return Ok.
///   2. `transport.read_frame()` returns `Closed` → idem.
///   3. Dispatch yields `ReplyAndShutdown(...)` → write reply, then
///      break.
///   4. I/O / Other transport error → log warn, break.
///
/// `InvalidFrame` is logged at warn and the loop continues so a
/// single malformed line never tears down a long-running session.
pub(crate) async fn server_loop<T, H>(
    transport: &mut T,
    dispatcher: &Dispatcher<H>,
    shutdown: CancellationToken,
) -> std::io::Result<()>
where
    T: McpTransport + ?Sized,
    H: McpServerHandler + 'static,
{
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            frame_result = transport.read_frame() => {
                let frame = match frame_result {
                    Ok(f) => f,
                    Err(TransportError::Closed) => break,
                    Err(TransportError::InvalidFrame(msg)) => {
                        tracing::warn!(error = %msg, "invalid jsonrpc frame; skipping");
                        continue;
                    }
                    Err(TransportError::Io(e)) => {
                        tracing::warn!(error = %e, "transport io error");
                        break;
                    }
                    Err(TransportError::Other(msg)) => {
                        tracing::warn!(error = %msg, "transport error");
                        break;
                    }
                };
                let request = frame.payload;
                let method = request
                    .get("method")
                    .and_then(|m| m.as_str())
                    .unwrap_or("");
                let params = request
                    .get("params")
                    .cloned()
                    .unwrap_or(Value::Null);
                let id = request.get("id").cloned();

                tracing::debug!(
                    method,
                    id = %id.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                    "mcp request received"
                );

                let cancel = shutdown.child_token();
                // Phase 76.3 — stdio is process-trusted; populate
                // `principal` with the constant `stdio_local()`
                // identity so 76.4 / 76.11 see a uniform field.
                let ctx = DispatchContext {
                    session_id: None,
                    request_id: id.clone(),
                    cancel,
                    principal: Some(crate::server::auth::Principal::stdio_local()),
                    progress_token: None,
                    session_sink: None,
                    correlation_id: None,
                };
                let method_owned = method.to_string();
                let outcome = dispatcher.dispatch(&method_owned, params, &ctx).await;
                let should_shutdown = matches!(outcome, DispatchOutcome::ReplyAndShutdown(_));

                if let Some(id_value) = id {
                    let response = match outcome {
                        DispatchOutcome::Reply(result) => serde_json::json!({
                            "jsonrpc": "2.0",
                            "result": result,
                            "id": id_value,
                        }),
                        DispatchOutcome::ReplyAndShutdown(result) => serde_json::json!({
                            "jsonrpc": "2.0",
                            "result": result,
                            "id": id_value,
                        }),
                        DispatchOutcome::Error { code, message, data } => {
                            let mut err = serde_json::json!({"code": code, "message": message});
                            if let Some(d) = data {
                                err.as_object_mut().unwrap().insert("data".into(), d);
                            }
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "error": err,
                                "id": id_value,
                            })
                        }
                        DispatchOutcome::Cancelled => serde_json::json!({
                            "jsonrpc": "2.0",
                            "error": {"code": -32800, "message": "request cancelled"},
                            "id": id_value,
                        }),
                        DispatchOutcome::Silent => continue,
                    };
                    if let Err(e) = transport.write_frame(Frame::new(response)).await {
                        tracing::warn!(error = %e, "transport write failed");
                        break;
                    }
                } else {
                    // Notification — no response even on error / cancel.
                    match outcome {
                        DispatchOutcome::Error { code, message, .. } => {
                            tracing::debug!(
                                method = %method_owned,
                                code,
                                %message,
                                "notification produced error; ignored"
                            );
                        }
                        DispatchOutcome::Cancelled => {
                            tracing::debug!(
                                method = %method_owned,
                                "notification cancelled; ignored"
                            );
                        }
                        _ => {}
                    }
                }
                if should_shutdown {
                    break;
                }
            }
        }
    }
    let _ = transport.close().await;
    Ok(())
}
