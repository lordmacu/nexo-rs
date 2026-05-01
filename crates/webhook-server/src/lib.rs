//! Phase 82.2 — axum-based HTTP server that hosts the webhook
//! receiver. Sits next to `nexo-webhook-receiver` (data plane) and
//! adds the wire layer plus the broker-backed dispatcher.
//!
//! Defense-in-depth pipeline (per-route handler):
//!   1. Method gate (axum router only matches POST).
//!   2. Body cap (`tower_http::limit::RequestBodyLimitLayer`).
//!   3. Per-source concurrency cap (`Arc<Semaphore>` permit).
//!   4. Per-source rate limit (`(source_id, client_ip)` bucket).
//!   5. `WebhookHandler::handle` (signature verify + event-kind
//!      extract + payload render).
//!   6. Dispatch to `WebhookDispatcher` impl (default: broker).
//!
//! Provider-agnostic by construction — every behaviour lookup is
//! data-driven via `WebhookServerConfig` resolvers.

#![deny(missing_docs)]

pub mod broker_dispatcher;
pub mod rate_limit;
pub mod reload;
pub mod router;
pub mod server;

pub use broker_dispatcher::BrokerWebhookDispatcher;
pub use rate_limit::{ClientBucketKey, ClientBucketMap, TokenBucket};
pub use reload::{reevaluate, EvictedSource, EvictionReason, ReevaluateReport};
pub use router::{build_router, RouterState, WebhookRouterError};
pub use server::{spawn_server, WebhookServerHandle};
