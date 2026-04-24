//! Errors returned by the MCP client.

use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("spawn failed: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("connect timed out after {0:?}")]
    ConnectTimeout(Duration),

    #[error("initialize timed out after {0:?}")]
    InitializeTimeout(Duration),

    #[error("call timed out after {0:?}")]
    CallTimeout(Duration),

    #[error("not ready: {0}")]
    NotReady(String),

    #[error("circuit breaker open")]
    BreakerOpen,

    #[error("child exited before handshake completed")]
    EarlyExit,

    #[error("child exited during call")]
    ChildExited,

    #[error("server error {code}: {message}")]
    ServerError { code: i32, message: String },

    #[error("protocol: {0}")]
    Protocol(String),

    #[error("encode: {0}")]
    Encode(#[source] serde_json::Error),

    #[error("decode: {0}")]
    Decode(#[source] serde_json::Error),

    #[error("transport I/O: {0}")]
    Transport(#[source] std::io::Error),

    #[error("transport lost: {0}")]
    TransportLost(String),

    #[error("http status {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("http: {0}")]
    Http(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_context() {
        let e = McpError::ServerError {
            code: -1,
            message: "nope".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("-1"));
        assert!(s.contains("nope"));
    }

    #[test]
    fn timeout_display_shows_duration() {
        let e = McpError::CallTimeout(Duration::from_secs(5));
        assert!(format!("{e}").contains("5s"));
    }
}
