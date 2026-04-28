//! Hand-rolled JSON-RPC framing over the LSP wire format.
//!
//! Every LSP message is `Content-Length: N\r\n\r\n<N-byte JSON>`.
//! Servers occasionally include an extra `Content-Type: ...\r\n`
//! header (the spec allows it, tsserver in some setups emits it);
//! the codec skips unknown headers but enforces presence of
//! `Content-Length`.
//!
//! The codec body uses [`tokio_util::codec::Decoder`] /
//! [`tokio_util::codec::Encoder`] so the [`LspClient`] in step 5
//! can wrap the child stdio with `Framed::new` and pull
//! `serde_json::Value` items.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/services/lsp/LSPClient.ts:1-9` — the
//!     leak imports `vscode-jsonrpc/node.js`'s `StreamMessageReader`
//!     / `StreamMessageWriter`. They implement exactly this wire
//!     format. We re-implement here in ~150 LoC because we are
//!     not in the JS ecosystem and `vscode-jsonrpc` is not a Rust
//!     crate.
//!
//! Reference (secondary):
//!   * `research/src/agents/pi-bundle-lsp-runtime.ts:46-86` —
//!     OpenClaw's `encodeLspMessage` + `parseLspMessages` are the
//!     direct prior art (TypeScript, ~40 LoC). Their parser uses
//!     buffer-of-strings instead of bytes which mishandles multi-byte
//!     UTF-8 boundaries; we work in bytes.
//!
//! [`LspClient`]: crate::client

use bytes::{Buf, BufMut, BytesMut};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

/// Codec for LSP `Content-Length` framing.
///
/// `Decoder<Item = serde_json::Value>` — yields parsed JSON
/// payloads; the caller (`LspClient`) routes them to either the
/// pending-request map (when `id` is set) or the notification
/// handler (when only `method` is set).
///
/// `Encoder<serde_json::Value>` — writes `Content-Length: N\r\n\r\n`
/// followed by the UTF-8 JSON body. The byte count is computed
/// from `serde_json::to_vec(&v)`, not `to_string` then `.len()`.
#[derive(Default, Debug)]
pub struct LspCodec;

impl LspCodec {
    pub const HEADER_TERMINATOR: &'static [u8] = b"\r\n\r\n";
    pub const CONTENT_LENGTH_PREFIX: &'static [u8] = b"Content-Length:";

    /// Inspect the buffer for a complete header block followed by
    /// `content_length` body bytes. Returns `Some(header_len,
    /// body_len)` when ready, `None` when more bytes are needed,
    /// `Err` when the header is malformed (no Content-Length).
    fn parse_header(src: &[u8]) -> Result<Option<(usize, usize)>, io::Error> {
        // Search for the `\r\n\r\n` terminator. Bounded by the
        // `MAX_HEADER_BYTES` cap so a malicious server cannot make
        // us scan an unbounded buffer per-poll.
        const MAX_HEADER_BYTES: usize = 8 * 1024;
        let scan_end = src.len().min(MAX_HEADER_BYTES);
        let term_at = match find_subsequence(&src[..scan_end], Self::HEADER_TERMINATOR) {
            Some(pos) => pos,
            None => {
                if scan_end == MAX_HEADER_BYTES && src.len() >= MAX_HEADER_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "LSP header exceeds 8 KiB without `\\r\\n\\r\\n` terminator",
                    ));
                }
                return Ok(None);
            }
        };

        let header_block = &src[..term_at];
        // Walk header lines (`\r\n`-separated) — pick the first
        // `Content-Length:` (case-insensitive on the field name per
        // RFC 7230, but the spec uses the canonical case).
        let mut content_length: Option<usize> = None;
        for line in header_block.split(|b| *b == b'\n') {
            // Trim trailing `\r`.
            let line = if line.last() == Some(&b'\r') {
                &line[..line.len() - 1]
            } else {
                line
            };
            if line.is_empty() {
                continue;
            }
            // Field name is up to ':'. Case-insensitive match.
            let colon = match line.iter().position(|b| *b == b':') {
                Some(pos) => pos,
                None => continue,
            };
            let name = &line[..colon];
            if name.eq_ignore_ascii_case(b"Content-Length") {
                let value_bytes = &line[colon + 1..];
                let value_str = std::str::from_utf8(value_bytes)
                    .map_err(|_| invalid("Content-Length value not UTF-8"))?
                    .trim();
                let n: usize = value_str
                    .parse()
                    .map_err(|_| invalid("Content-Length not a non-negative integer"))?;
                content_length = Some(n);
                break;
            }
            // Other headers (Content-Type, etc.) are skipped silently.
        }

        let header_len = term_at + Self::HEADER_TERMINATOR.len();
        match content_length {
            Some(n) => Ok(Some((header_len, n))),
            None => Err(invalid("LSP header missing Content-Length")),
        }
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl Decoder for LspCodec {
    type Item = serde_json::Value;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let (header_len, body_len) = match Self::parse_header(src)? {
            Some(pair) => pair,
            None => return Ok(None),
        };
        let total = header_len + body_len;
        if src.len() < total {
            // Reserve so the BufRead loop fills in one go.
            src.reserve(total - src.len());
            return Ok(None);
        }
        // Drop header.
        src.advance(header_len);
        // Take exactly body_len bytes.
        let body = src.split_to(body_len);
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("LSP body is not valid JSON: {e}"),
            )
        })?;
        Ok(Some(value))
    }
}

impl Encoder<serde_json::Value> for LspCodec {
    type Error = io::Error;

    fn encode(&mut self, item: serde_json::Value, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let body = serde_json::to_vec(&item).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("LSP encode: serialise JSON failed: {e}"),
            )
        })?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        dst.reserve(header.len() + body.len());
        dst.put_slice(header.as_bytes());
        dst.put_slice(&body);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use serde_json::json;

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn roundtrip_simple_request() {
        let value = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let mut codec = LspCodec;
        let mut buf = BytesMut::new();
        codec.encode(value.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap();
        assert_eq!(decoded, Some(value));
        assert!(buf.is_empty());
    }

    #[test]
    fn roundtrip_unicode_payload() {
        // 4-byte UTF-8 (emoji) + 2-byte (cyrillic) + ASCII. Encoder
        // must compute Content-Length in BYTES, not chars.
        let value = json!({"text": "Привет 🌍 hello"});
        let mut codec = LspCodec;
        let mut buf = BytesMut::new();
        codec.encode(value.clone(), &mut buf).unwrap();

        // Header should reflect the UTF-8 byte count.
        let body_str = serde_json::to_string(&value).unwrap();
        let expected = format!("Content-Length: {}\r\n\r\n", body_str.len());
        assert!(buf.starts_with(expected.as_bytes()));

        let decoded = codec.decode(&mut buf).unwrap();
        assert_eq!(decoded, Some(value));
    }

    #[test]
    fn decode_split_across_reads() {
        let body = br#"{"jsonrpc":"2.0","id":7,"result":null}"#;
        let framed = frame(body);
        let mut codec = LspCodec;
        let mut buf = BytesMut::new();

        // Read 1: just part of the header.
        buf.extend_from_slice(&framed[..10]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);

        // Read 2: rest of the header + part of the body.
        let body_start = framed.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        buf.extend_from_slice(&framed[10..body_start + 5]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);

        // Read 3: rest of the body.
        buf.extend_from_slice(&framed[body_start + 5..]);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded["id"], 7);
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_two_messages_in_one_buf() {
        let body1 = br#"{"id":1}"#;
        let body2 = br#"{"id":2}"#;
        let mut combined = frame(body1);
        combined.extend(frame(body2));
        let mut codec = LspCodec;
        let mut buf = BytesMut::from(&combined[..]);

        let m1 = codec.decode(&mut buf).unwrap().unwrap();
        let m2 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(m1["id"], 1);
        assert_eq!(m2["id"], 2);
        assert!(buf.is_empty());
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
    }

    #[test]
    fn decode_extra_headers_skipped() {
        let body = br#"{"id":42}"#;
        let mut framed = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             X-Server-Note: rust-analyzer\r\n\r\n",
            body.len()
        )
        .into_bytes();
        framed.extend_from_slice(body);
        let mut codec = LspCodec;
        let mut buf = BytesMut::from(&framed[..]);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded["id"], 42);
    }

    #[test]
    fn decode_missing_content_length_is_err() {
        let mut buf = BytesMut::from(&b"X-Foo: bar\r\n\r\n{}"[..]);
        let err = LspCodec.decode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("Content-Length"));
    }

    #[test]
    fn decode_malformed_body_returns_err() {
        let body = br#"{not_valid_json"#;
        let framed = frame(body);
        let mut buf = BytesMut::from(&framed[..]);
        let err = LspCodec.decode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("not valid JSON"));
    }

    #[test]
    fn decode_oversized_header_is_err() {
        // > 8 KiB without `\r\n\r\n` should error to prevent a
        // malicious server from making us scan unbounded.
        let big = vec![b'A'; 9 * 1024];
        let mut buf = BytesMut::from(&big[..]);
        let err = LspCodec.decode(&mut buf).unwrap_err();
        assert!(err.to_string().contains("8 KiB"));
    }

    #[test]
    fn case_insensitive_content_length_field_name() {
        let body = br#"{"id":3}"#;
        let mut framed = format!("content-length: {}\r\n\r\n", body.len()).into_bytes();
        framed.extend_from_slice(body);
        let mut buf = BytesMut::from(&framed[..]);
        let decoded = LspCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded["id"], 3);
    }
}
