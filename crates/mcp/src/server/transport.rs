//! Phase 76.2 — transport abstraction shared by the stdio server and
//! the future HTTP server (76.1). Each transport impl owns its
//! framing rules (newline-delimited JSON for stdio, body+SSE for
//! HTTP) and yields parsed JSON `Value`s upstream. The dispatcher in
//! `super::dispatch` is transport-agnostic: it never sees frames, it
//! only sees parsed JSON-RPC method/params and emits a typed
//! `DispatchOutcome` that the transport renders on the wire.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

/// Errors that any `McpTransport` may surface to the driver loop.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Peer closed the connection cleanly. Not strictly an error;
    /// the driver loop should break out and exit normally.
    #[error("peer closed")]
    Closed,
    /// Underlying I/O failure (read/write/flush).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// A frame was received but the bytes were not valid JSON. The
    /// driver may choose to skip-and-continue (stdio precedent) or
    /// surface a 400 (HTTP). Carries the raw error message for
    /// diagnostics.
    #[error("invalid frame: {0}")]
    InvalidFrame(String),
    /// Anything else the transport wants to surface (e.g. session
    /// not found, route closed, backpressure overflow).
    #[error("transport: {0}")]
    Other(String),
}

/// One inbound or outbound JSON-RPC message plus an opaque routing
/// hint. For stdio every frame is independent and `route` is `None`.
/// For HTTP the read side fills `route` with a per-request id so the
/// write side can match the response back to the originating HTTP
/// call (used in 76.1).
#[derive(Debug, Clone)]
pub struct Frame {
    /// Parsed JSON-RPC envelope. Validated as JSON; NOT yet
    /// validated as JSON-RPC — that is the dispatcher's job.
    pub payload: Value,
    /// Opaque destination key. Transports define their own scheme;
    /// the dispatcher never reads this.
    pub route: Option<String>,
}

impl Frame {
    /// Frame without routing metadata (stdio default).
    pub fn new(payload: Value) -> Self {
        Self {
            payload,
            route: None,
        }
    }

    /// Frame with a transport-defined route key.
    pub fn with_route(payload: Value, route: String) -> Self {
        Self {
            payload,
            route: Some(route),
        }
    }
}

/// Transport contract. Every concrete impl must be `Send` so the
/// driver loop can move it across `await` points; `Sync` is NOT
/// required because the driver always owns the transport mutably
/// and never shares it across tasks.
#[async_trait]
pub trait McpTransport: Send {
    /// Block until the next inbound frame arrives. Returns
    /// `Err(TransportError::Closed)` on clean EOF; the driver loop
    /// must terminate when this fires.
    async fn read_frame(&mut self) -> Result<Frame, TransportError>;

    /// Write one outbound frame. The transport interprets `route`
    /// (when present) to pick the correct response sender / SSE
    /// channel. Stdio ignores `route` entirely.
    async fn write_frame(&mut self, frame: Frame) -> Result<(), TransportError>;

    /// Idempotent best-effort shutdown. After this call, both
    /// `read_frame` and `write_frame` MUST return
    /// `TransportError::Closed`.
    async fn close(&mut self) -> Result<(), TransportError>;
}
