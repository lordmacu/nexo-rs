//! Plugin catalogue discovery (Phase 98).
//!
//! Fetches public plugin metadata from three sources, merges by
//! `crate_name`, derives compat + trust tier, caches to disk with
//! 24-hour TTL. Built as a standalone crate so a future microapp
//! UI mirror (Phase 98 deferred #6) can reuse it without depending
//! on the daemon.
//!
//! Top-level surface:
//!   - [`config::DiscoveryConfig`] — bake-in defaults + per-deployment overrides.
//!   - [`cache::DiskCache`] — atomic temp+rename writer with TTL gate.
//!   - [`types`] — typed payload re-exports of the wire shapes shipped
//!     by `nexo_tool_meta::admin::plugin_discovery` (98.5), with
//!     construction helpers + serialization-friendly defaults.

#![deny(missing_docs)]
#![warn(rust_2018_idioms, unreachable_pub)]

pub mod cache;
pub mod config;
pub mod types;
