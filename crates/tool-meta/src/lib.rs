//! Wire-shape types shared between the Nexo agent runtime and any
//! third-party microapp that consumes its events.
//!
//! Provider-agnostic by construction: no transport layer (no `axum`,
//! no `tokio`), no broker, no agent runtime. Pulling this crate in
//! is a four-dependency, sub-second compile and exposes only the
//! data shapes a downstream consumer needs to read what nexo emits.
//!
//! See the modules below for the concrete types.
