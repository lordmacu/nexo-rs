//! Minimal Server-Sent Events parser for MCP transport.
//!
//! MCP uses only three event shapes: `event: endpoint` (SSE legacy bootstrap),
//! unnamed `data:` blocks carrying JSON-RPC messages, and keepalive comment
//! lines. We parse manually rather than pull in `reqwest-eventsource` to
//! keep the dep surface small.

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) enum SseEvent {
    /// Legacy SSE bootstrap: `event: endpoint\ndata: https://.../post\n\n`
    Endpoint(String),
    /// Unnamed `data:` block carrying a JSON-RPC message.
    Message(serde_json::Value),
}

/// Consume `text/event-stream` bytes from `response`, emit parsed events.
/// Loop exits on EOF, shutdown, or parse error. Parse errors on individual
/// events are logged and skipped; the stream keeps going.
pub(crate) async fn run_sse_reader<S>(
    mut byte_stream: S,
    event_tx: mpsc::Sender<SseEvent>,
    shutdown: CancellationToken,
    log_name: String,
) where
    S: futures_util::stream::Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin,
{
    let mut buffer = String::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            chunk = byte_stream.next() => {
                match chunk {
                    Some(Ok(bytes)) => {
                        match std::str::from_utf8(&bytes) {
                            Ok(s) => buffer.push_str(s),
                            Err(e) => {
                                tracing::warn!(mcp=%log_name, error=%e, "sse: non-utf8 chunk");
                                continue;
                            }
                        }
                        flush_events(&mut buffer, &event_tx, &log_name).await;
                    }
                    Some(Err(e)) => {
                        tracing::warn!(mcp=%log_name, error=%e, "sse: stream error");
                        break;
                    }
                    None => {
                        tracing::info!(mcp=%log_name, "sse: stream closed");
                        break;
                    }
                }
            }
        }
    }
}

async fn flush_events(buffer: &mut String, tx: &mpsc::Sender<SseEvent>, log_name: &str) {
    while let Some(idx) = find_double_newline(buffer) {
        let block = buffer[..idx].to_string();
        buffer.drain(..idx + 2);
        if let Some(event) = parse_block(&block, log_name) {
            if tx.send(event).await.is_err() {
                return; // receiver dropped
            }
        }
    }
}

fn find_double_newline(s: &str) -> Option<usize> {
    // Accept `\n\n` and `\r\n\r\n` equivalents. SSE spec allows both.
    if let Some(i) = s.find("\n\n") {
        return Some(i);
    }
    if let Some(i) = s.find("\r\n\r\n") {
        return Some(i);
    }
    None
}

fn parse_block(block: &str, log_name: &str) -> Option<SseEvent> {
    let mut event_kind: Option<&str> = None;
    let mut data_lines: Vec<&str> = Vec::new();

    for line in block.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue; // blank or comment
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => event_kind = Some(value_static(value)),
            "data" => data_lines.push(value_owned(value)),
            _ => {}
        }
    }
    // Reconstruct combined data.
    let data = data_lines.join("\n");
    match event_kind {
        Some("endpoint") => {
            let trimmed = data.trim();
            if trimmed.is_empty() {
                tracing::warn!(mcp=%log_name, "sse: empty endpoint event");
                None
            } else {
                Some(SseEvent::Endpoint(trimmed.to_string()))
            }
        }
        Some(other) if other != "message" => {
            tracing::debug!(mcp=%log_name, event=%other, "sse: ignoring event");
            None
        }
        _ => {
            // Unnamed or `message` event.
            if data.is_empty() {
                return None;
            }
            match serde_json::from_str::<serde_json::Value>(&data) {
                Ok(v) => Some(SseEvent::Message(v)),
                Err(e) => {
                    tracing::warn!(mcp=%log_name, error=%e, raw=%data, "sse: invalid json");
                    None
                }
            }
        }
    }
}

// Helpers so parse_block can keep references when possible but still compile
// cleanly. SSE event names are short; clone is acceptable.
fn value_static(v: &str) -> &'static str {
    match v {
        "endpoint" => "endpoint",
        "message" => "message",
        "ping" => "ping",
        _ => "other",
    }
}

fn value_owned(v: &str) -> &str {
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    async fn drain(bytes: &'static [u8]) -> Vec<SseEvent> {
        let chunks = vec![Ok::<Bytes, reqwest::Error>(Bytes::from_static(bytes))];
        let stream = stream::iter(chunks);
        let (tx, mut rx) = mpsc::channel(16);
        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(run_sse_reader(stream, tx, shutdown, "test".into()));
        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await {
            out.push(ev);
        }
        handle.await.unwrap();
        out
    }

    #[tokio::test]
    async fn parses_endpoint_event() {
        let out = drain(b"event: endpoint\ndata: https://x.example/post\n\n").await;
        assert_eq!(out.len(), 1);
        match &out[0] {
            SseEvent::Endpoint(url) => assert_eq!(url, "https://x.example/post"),
            _ => panic!("expected Endpoint"),
        }
    }

    #[tokio::test]
    async fn parses_multiple_message_blocks() {
        let out = drain(b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\n").await;
        assert_eq!(out.len(), 2);
        for ev in &out {
            match ev {
                SseEvent::Message(_) => {}
                _ => panic!(),
            }
        }
    }

    #[tokio::test]
    async fn skips_comment_lines_and_unknown_events() {
        let out =
            drain(b": keepalive\nevent: custom\ndata: skip-me\n\ndata: {\"hello\":\"world\"}\n\n")
                .await;
        assert_eq!(out.len(), 1);
        match &out[0] {
            SseEvent::Message(v) => assert_eq!(v["hello"], "world"),
            _ => panic!(),
        }
    }
}
