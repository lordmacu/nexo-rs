//! Phase 12.2 — HTTP transport (streamable-http + SSE legacy).

pub mod client;
pub mod redact;
pub(crate) mod sse;

pub use client::{HttpMcpClient, HttpMcpOptions, HttpTransportMode};
pub use redact::redact_sensitive_url;
