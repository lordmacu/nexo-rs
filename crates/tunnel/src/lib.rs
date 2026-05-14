//! Public-HTTPS tunnel for Nexo agents — **legacy alias**.
//!
//! As of Phase 92.8 the canonical home is
//! [`nexo-tunnel-quick`](https://crates.io/crates/nexo-tunnel-quick).
//! This crate is now a transparent re-export so daemons + CLI tools
//! that import `nexo_tunnel::{TunnelManager, TunnelHandle, ...}`
//! keep building unchanged. Phase 92.11 will retire this alias.
//!
//! Prefer importing from `nexo_tunnel_quick` in new code.

pub use nexo_tunnel_quick::*;
