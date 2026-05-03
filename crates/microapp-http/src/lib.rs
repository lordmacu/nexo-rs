//! `nexo-microapp-http` — reusable HTTP scaffolding for nexo-rs
//! microapps with operator UIs.
//!
//! Lifted from the `agent-creator-microapp` reference implementation.
//! Every microapp declaring `[capabilities.http_server]` needs the
//! same building blocks: bearer auth, an `AdminClient` proxy, an SPA
//! static-asset router, and a runtime-mutable token state that
//! survives `nexo/notify/token_rotated` swaps without restarts.
//!
//! See module-level docs for each piece.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod admin_proxy;
pub mod auth;
pub mod error;
pub mod sse;
pub mod static_assets;
