//! Wire-shape types shared between the Nexo agent runtime and any
//! third-party microapp that consumes its events.
//!
//! Provider-agnostic by construction: no transport layer (no `axum`,
//! no `tokio`), no broker, no agent runtime. Pulling this crate in
//! is a four-dependency, sub-second compile and exposes only the
//! data shapes a downstream consumer needs to read what nexo emits.
//!
//! See the modules below for the concrete types.

pub mod binding;
pub mod meta;
pub mod webhook;

pub use binding::{binding_id_render, BindingContext};
pub use meta::{build_meta_value, parse_binding_from_meta, BINDING_KEY, META_KEY, NEXO_NAMESPACE};
pub use webhook::{format_webhook_source, WebhookEnvelope, ENVELOPE_SCHEMA_VERSION};
