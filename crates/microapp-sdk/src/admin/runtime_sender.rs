//! Phase 83.8.8.a — `WriterAdminSender` bridges the SDK runtime's
//! shared `Arc<Mutex<W>>` stdout writer to the
//! [`AdminSender`] trait the [`AdminClient`] consumes.
//!
//! Lets a microapp share a single line-oriented writer between
//! the daemon-initiated reply path (tool/hook responses) and the
//! microapp-initiated request path (admin RPC) without
//! interleaving partial frames.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

use super::{AdminError, AdminSender};

/// Bridge from the shared `Arc<Mutex<W>>` line writer used by the
/// SDK runtime to the [`AdminSender`] surface that
/// [`super::AdminClient`] consumes. The trait is type-erased
/// (`dyn AdminSender`) so callers do not need to thread the `W`
/// generic through their handler closures.
pub struct WriterAdminSender<W>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    writer: Arc<Mutex<W>>,
}

impl<W> std::fmt::Debug for WriterAdminSender<W>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterAdminSender").finish_non_exhaustive()
    }
}

impl<W> WriterAdminSender<W>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    /// Wrap a writer the SDK runtime already owns. The shared
    /// mutex serialises writes against the daemon-reply path so
    /// admin frames never interleave with tool/hook responses.
    pub fn new(writer: Arc<Mutex<W>>) -> Self {
        Self { writer }
    }
}

#[async_trait]
impl<W> AdminSender for WriterAdminSender<W>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    async fn send_line(&self, line: String) -> Result<(), AdminError> {
        let mut buf = line;
        buf.push('\n');
        let mut guard = self.writer.lock().await;
        guard
            .write_all(buf.as_bytes())
            .await
            .map_err(|e| AdminError::Transport(e.to_string()))?;
        guard
            .flush()
            .await
            .map_err(|e| AdminError::Transport(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Microapp, ToolCtx, ToolError, ToolReply};
    use serde_json::Value;
    use tokio::io::DuplexStream;

    #[tokio::test]
    async fn writer_admin_sender_round_trips_one_line() {
        let (write_half, mut read_half) = tokio::io::duplex(1024);
        let writer = Arc::new(Mutex::new(write_half));
        let sender = WriterAdminSender::new(writer);
        sender.send_line("hello".into()).await.unwrap();
        let mut buf = vec![0u8; 16];
        use tokio::io::AsyncReadExt;
        let n = read_half.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello\n");
    }

    /// End-to-end: a tool that calls `ctx.admin().call(...)` issues
    /// a `nexo/admin/<method>` frame on stdout, and an `app:`
    /// correlated response read from stdin lands back in the
    /// AdminClient pending map.
    #[tokio::test]
    async fn admin_round_trip_via_runtime() {
        async fn echo_via_admin(_args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
            let admin = ctx.admin().expect("admin wired");
            let v: Value = admin
                .call_raw("nexo/admin/echo", serde_json::json!({"x": 1}))
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;
            Ok(ToolReply::ok_json(v))
        }

        let app = Microapp::new("test", "0.0.0")
            .with_admin()
            .with_tool("call_admin", echo_via_admin);

        let (client_to_app, app_stdin) = tokio::io::duplex(1024);
        let (app_stdout, mut client_from_app) = tokio::io::duplex(1024);

        let runner = tokio::spawn(async move {
            app.run_with(tokio::io::BufReader::new(app_stdin), app_stdout).await
        });

        // 1. Call the tool — runtime invokes echo_via_admin.
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "call_admin", "arguments": {} },
        });
        use tokio::io::AsyncWriteExt;
        let mut stdin = client_to_app;
        let line = format!("{req}\n");
        stdin.write_all(line.as_bytes()).await.unwrap();
        stdin.flush().await.unwrap();

        // 2. Read the admin frame the tool emitted.
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 1024];
        let n = client_from_app.read(&mut buf).await.unwrap();
        let admin_frame: Value =
            serde_json::from_slice(buf[..n].split(|b| *b == b'\n').next().unwrap()).unwrap();
        assert_eq!(admin_frame["method"], "nexo/admin/echo");
        let admin_id = admin_frame["id"].as_str().unwrap().to_string();
        assert!(admin_id.starts_with("app:"));

        // 3. Synthesise the daemon's admin response.
        let admin_resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": admin_id,
            "result": { "echoed": { "x": 1 } },
        });
        let line = format!("{admin_resp}\n");
        stdin.write_all(line.as_bytes()).await.unwrap();
        stdin.flush().await.unwrap();

        // 4. Read the tool reply.
        let mut buf2 = vec![0u8; 1024];
        let mut full = Vec::new();
        loop {
            let n = client_from_app.read(&mut buf2).await.unwrap();
            if n == 0 {
                break;
            }
            full.extend_from_slice(&buf2[..n]);
            // Stop once we got the second frame.
            if full.iter().filter(|b| **b == b'\n').count() >= 1 {
                break;
            }
        }
        let tool_reply: Value = serde_json::from_slice(
            full.split(|b| *b == b'\n').next().unwrap(),
        )
        .unwrap();
        assert_eq!(tool_reply["id"], serde_json::json!(1));
        // Cleanup — drop stdin to break the loop.
        drop(stdin);
        drop(client_from_app);
        runner.abort();
        let _ = runner.await;
    }

    #[tokio::test]
    async fn writer_admin_sender_serialises_concurrent_lines() {
        let (write_half, mut read_half) = tokio::io::duplex(1024);
        let writer: Arc<Mutex<DuplexStream>> = Arc::new(Mutex::new(write_half));
        let s1 = WriterAdminSender::new(writer.clone());
        let s2 = WriterAdminSender::new(writer.clone());
        // Fire two sends in parallel; each MUST land as one
        // contiguous line, never interleaved.
        let t1 = tokio::spawn(async move { s1.send_line("AAAA".into()).await });
        let t2 = tokio::spawn(async move { s2.send_line("BBBB".into()).await });
        t1.await.unwrap().unwrap();
        t2.await.unwrap().unwrap();
        let mut text = String::new();
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 32];
        let n = read_half.read(&mut buf).await.unwrap();
        text.push_str(std::str::from_utf8(&buf[..n]).unwrap());
        // We expect two complete lines, regardless of order.
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort();
        assert_eq!(lines, vec!["AAAA", "BBBB"]);
    }
}
