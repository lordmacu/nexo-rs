//! Outbound dispatcher (feature `outbound`).
//!
//! Typed API for `nexo/dispatch` JSON-RPC requests the microapp
//! sends to the daemon. **v0 stub** — the daemon-side runtime
//! lands with Phase 82.3.b; until then every method returns
//! [`DispatchError::Transport`] with a clear "82.3.b runtime not
//! ready" message so calling code is forced to handle the
//! error path.

use thiserror::Error;

/// Acknowledgment returned by a successful dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchAck {
    /// Provider-stamped message id, when available.
    pub msg_id: Option<String>,
}

/// Reasons a dispatch can fail.
///
/// Operator-facing diagnostic — `#[non_exhaustive]` so future
/// failure modes (e.g. `IdempotencyConflict`) land as semver-minor.
#[non_exhaustive]
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// `(channel, account_id)` pair is not in the extension's
    /// declared `[outbound_bindings]` allowlist.
    #[error("unauthorized binding `{channel}:{account_id}`")]
    UnauthorizedBinding {
        /// Channel that was attempted.
        channel: String,
        /// Account id that was attempted.
        account_id: String,
    },
    /// No adapter registered for the requested channel.
    #[error("channel `{0}` adapter not registered")]
    ChannelUnavailable(String),
    /// Per-extension rate limit hit.
    #[error("rate limit exceeded")]
    RateLimitExceeded,
    /// Validation failed (missing required field, malformed
    /// input, etc.).
    #[error("validation error: {0}")]
    ValidationError(String),
    /// Transport-level failure — broker offline, JSON-RPC wire
    /// error, daemon-side panic.
    #[error("transport: {0}")]
    Transport(String),
}

/// Outbound dispatcher.
///
/// Constructed internally by the SDK; microapps access it via
/// `ctx.outbound()` from a tool handler.
#[derive(Debug, Clone)]
pub struct OutboundDispatcher {
    /// v0 stub flag. Future versions wire up the JSON-RPC
    /// request-from-extension transport; until then every method
    /// returns `Transport("82.3.b runtime not ready")`.
    stub: bool,
}

impl OutboundDispatcher {
    /// Build a stub dispatcher. v0 — every send returns
    /// `DispatchError::Transport`.
    pub fn new_stub() -> Self {
        Self { stub: true }
    }

    /// Dispatch a plain-text outbound message.
    ///
    /// v0: returns `DispatchError::Transport("82.3.b runtime not
    /// ready")`. Real impl arrives with Phase 82.3.b daemon
    /// support.
    pub async fn send_text(
        &self,
        channel: &str,
        account_id: &str,
        to: &str,
        text: &str,
    ) -> Result<DispatchAck, DispatchError> {
        let _ = (channel, account_id, to, text);
        if self.stub {
            return Err(DispatchError::Transport(
                "82.3.b runtime not ready".to_string(),
            ));
        }
        Ok(DispatchAck { msg_id: None })
    }

    /// Dispatch a media outbound message.
    ///
    /// v0 stub — see [`Self::send_text`].
    pub async fn send_media(
        &self,
        channel: &str,
        account_id: &str,
        to: &str,
        media_url: &str,
        caption: Option<&str>,
    ) -> Result<DispatchAck, DispatchError> {
        let _ = (channel, account_id, to, media_url, caption);
        if self.stub {
            return Err(DispatchError::Transport(
                "82.3.b runtime not ready".to_string(),
            ));
        }
        Ok(DispatchAck { msg_id: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_text_stub_returns_transport_error() {
        let d = OutboundDispatcher::new_stub();
        let r = d
            .send_text("whatsapp", "personal", "+5491100", "hi")
            .await;
        assert!(matches!(r, Err(DispatchError::Transport(_))));
    }

    #[tokio::test]
    async fn send_media_stub_returns_transport_error() {
        let d = OutboundDispatcher::new_stub();
        let r = d
            .send_media("whatsapp", "personal", "+5491100", "http://x", None)
            .await;
        assert!(matches!(r, Err(DispatchError::Transport(_))));
    }

    #[test]
    fn dispatch_error_unauthorized_binding_renders_message() {
        let e = DispatchError::UnauthorizedBinding {
            channel: "whatsapp".into(),
            account_id: "x".into(),
        };
        assert!(format!("{e}").contains("unauthorized binding"));
    }
}
