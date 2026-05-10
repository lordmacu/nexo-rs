//! Wire-shape types shared between the Nexo agent runtime and any
//! third-party microapp that consumes its events.
//!
//! Provider-agnostic by construction: no transport layer (no
//! `axum`, no `tokio`), no broker, no agent runtime. Pulling this
//! crate in is a four-dependency, sub-second compile and exposes
//! only the data shapes a downstream consumer needs to read what
//! the daemon emits.
//!
//! # What lives here
//!
//! - [`BindingContext`] — identifies the inbound binding a tool
//!   call originated from. Microapps read it from
//!   `params._meta.nexo.binding`.
//! - [`InboundMessageMeta`] — per-turn metadata about the inbound
//!   message that triggered the agent turn (sender id, msg id,
//!   timestamp, …). Microapps read it from
//!   `params._meta.nexo.inbound`.
//! - [`build_meta_value`] / [`parse_binding_from_meta`] /
//!   [`parse_inbound_from_meta`] — the inverse trio around the
//!   dual-write `_meta` payload.
//! - [`WebhookEnvelope`] — JSON envelope nexo publishes to NATS
//!   after every accepted webhook request.
//! - [`format_webhook_source`] — Phase 72 turn-log marker
//!   helper.
//!
//! # Round-trip example
//!
//! ```
//! use nexo_tool_meta::{build_meta_value, parse_binding_from_meta, BindingContext};
//! use uuid::Uuid;
//!
//! // Daemon side: build the `_meta` payload for a tool call.
//! let mut binding = BindingContext::agent_only("ana");
//! binding.channel = Some("whatsapp".into());
//! binding.account_id = Some("personal".into());
//! binding.binding_id = Some("whatsapp:personal".into());
//! let meta = build_meta_value("ana", Some(Uuid::nil()), Some(&binding), None);
//!
//! // Microapp side: extract the binding back.
//! let parsed = parse_binding_from_meta(&meta).unwrap();
//! assert_eq!(parsed.channel.as_deref(), Some("whatsapp"));
//! assert_eq!(parsed.binding_id.as_deref(), Some("whatsapp:personal"));
//! ```

#![deny(missing_docs)]

pub mod admin;
pub mod binding;
pub mod event_source;
pub mod http_server;
pub mod inbound;
/// Phase 81.19.b locale follow-up — moved from nexo-microapp-sdk so
/// nexo-config can consume the closed-enum validator at YAML
/// deserialise time. SDK still re-exports for back-compat.
pub mod locale;
pub mod marketing;
pub mod meta;
pub mod microapp_error;
pub mod reply_kind;
pub mod template;
pub mod webhook;

pub use admin::audit::{AdminAuditResult, AdminAuditRow, AuditTailFilter, AuditTailPage};
pub use binding::{binding_id_render, BindingContext};
pub use locale::{LangCode, Locale, LocaleParseError, RegionCode};
pub use event_source::{
    format_dispatch_source, format_event_subscriber_source, format_rate_limit_hit, EventSourceMeta,
};
pub use inbound::{InboundKind, InboundMessageMeta};
pub use meta::{
    build_meta_value, parse_binding_from_meta, parse_inbound_from_meta, BINDING_KEY, INBOUND_KEY,
    META_KEY, NEXO_NAMESPACE,
};
pub use microapp_error::{MicroappError, MicroappErrorKind, MICROAPP_ERROR_NOTIFY_METHOD};
pub use template::{render_template, MISSING_PLACEHOLDER};
pub use webhook::{format_webhook_source, WebhookEnvelope, ENVELOPE_SCHEMA_VERSION};

// Phase 83.12.ts-types-codegen — types tagged
// `#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]`
// auto-generate one test per type via the `ts_rs::TS` derive macro.
// Running `cargo test --features=ts-export -p nexo-tool-meta` triggers
// every test, which writes per-type `.ts` files into the directory
// pointed at by `TS_RS_EXPORT_DIR` (or `./bindings` by default). The
// wrapper script `proyecto/scripts/regen-ts-types.sh` sets the env
// var to point at agent-creator-microapp's frontend api/ directory
// and concatenates the per-type files into a single `types.gen.ts`
// with a banner header.
//
// Production builds (no `ts-export` feature) skip the derive
// entirely — zero runtime impact.
