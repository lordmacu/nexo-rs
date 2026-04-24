//! Shared reliability primitives for stdio extensions in `proyecto`.
//!
//! Extracted after the 4th copy of `breaker.rs` appeared (weather, openstreetmap,
//! github, summarize). Kept intentionally minimal — only utilities that are
//! identical across providers belong here.

pub mod breaker;

pub use breaker::{Breaker, BreakerError};
