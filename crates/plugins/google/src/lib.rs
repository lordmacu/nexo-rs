//! Google OAuth + `google_*` tools.
//!
//! Originally lived in `agent-core::agent::google_auth`. Extracted to
//! its own plugin crate so the runtime doesn't ship the OAuth machinery
//! when an agent has no `google_auth:` config block. See
//! `docs/layering.md` for the plugin / extension / skill split.

pub mod client;
pub mod tools;

pub use client::{
    canonicalize_scopes, DeviceChallenge, GoogleAuthClient, GoogleAuthConfig, GoogleTokens,
    SecretSources,
};
pub use tools::{
    register_all as register_tools, GoogleAuthRevokeTool, GoogleAuthStartTool,
    GoogleAuthStatusTool, GoogleCallTool,
};
