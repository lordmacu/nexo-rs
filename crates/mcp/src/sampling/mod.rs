//! MCP sampling support — `sampling/createMessage` handling.
//!
//! Servers send `sampling/createMessage` requests to the client when they
//! want the client's LLM to generate text. The client (this crate)
//! implements a [`SamplingProvider`] that satisfies those requests and
//! replies on the same JSON-RPC channel.
//!
//! The framework ships a [`DefaultSamplingProvider`] that wraps any
//! `agent_llm::LlmClient` and applies a [`SamplingPolicy`] (rate limit
//! + token cap + deny list) before forwarding.

pub mod default_provider;
pub mod policy;
pub mod types;
pub mod wire;

pub use default_provider::DefaultSamplingProvider;
pub use policy::SamplingPolicy;
pub use types::{
    IncludeContext, ModelPreferences, SamplingMessage, SamplingRequest, SamplingResponse,
    SamplingRole, StopReason,
};

use async_trait::async_trait;

/// Implementations handle `sampling/createMessage` requests forwarded
/// from a connected MCP server.
#[async_trait]
pub trait SamplingProvider: Send + Sync {
    async fn sample(&self, req: SamplingRequest) -> Result<SamplingResponse, SamplingError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SamplingError {
    #[error("sampling disabled for server '{0}'")]
    Disabled(String),
    #[error("rate limited (retry in {0}ms)")]
    RateLimited(u64),
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("tool_calls response rejected for sampling")]
    ToolCallsRejected,
    #[error("llm failure: {0}")]
    LlmError(String),
    #[error("no provider resolved from hints")]
    NoProvider,
}

impl SamplingError {
    /// Map to JSON-RPC error code (per spec: application errors use
    /// `-32000` range, `-32602` for invalid params, `-32603` for internal).
    pub fn json_rpc_code(&self) -> i32 {
        match self {
            Self::Disabled(_) | Self::RateLimited(_) => -32000,
            Self::InvalidParams(_) => -32602,
            Self::ToolCallsRejected | Self::LlmError(_) | Self::NoProvider => -32603,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_match_spec() {
        assert_eq!(SamplingError::Disabled("x".into()).json_rpc_code(), -32000);
        assert_eq!(SamplingError::RateLimited(1000).json_rpc_code(), -32000);
        assert_eq!(
            SamplingError::InvalidParams("x".into()).json_rpc_code(),
            -32602
        );
        assert_eq!(SamplingError::ToolCallsRejected.json_rpc_code(), -32603);
        assert_eq!(SamplingError::LlmError("x".into()).json_rpc_code(), -32603);
        assert_eq!(SamplingError::NoProvider.json_rpc_code(), -32603);
    }
}
