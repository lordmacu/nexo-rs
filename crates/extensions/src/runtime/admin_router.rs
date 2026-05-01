//! Phase 82.10.h.b.3 — extension-host hook for routing inbound
//! `app:`-prefixed JSON-RPC frames to an admin RPC dispatcher.
//!
//! The reader task in [`super::stdio`] receives every line from
//! the extension's stdout. Frames whose `id` is a string starting
//! with `app:` are microapp-initiated admin requests (per the
//! Phase 82.10 wire contract); they are NOT responses to
//! daemon-initiated `tools/call` and must NOT be matched against
//! the pending-request map. The reader hands them to an
//! [`AdminRouter`] instead.
//!
//! The trait lives here (consumer side) rather than in
//! `nexo-core` to avoid the dep direction inverting — `nexo-core`
//! already depends on `nexo-extensions`. The concrete dispatcher
//! adapter that implements this trait ships in
//! `nexo_core::agent::admin_rpc::router`.

use std::sync::Arc;

use async_trait::async_trait;

/// Router interface registered per-extension at spawn time. The
/// reader task forwards one whole JSON-RPC frame line at a time;
/// the implementation owns parsing + dispatch + response delivery
/// (typically by writing back through the same stdin outbox the
/// host already manages).
///
/// Implementations must be safe to call concurrently — the reader
/// task spawns an `await` per frame, so multiple in-flight admin
/// calls from the same microapp can interleave.
#[async_trait]
pub trait AdminRouter: Send + Sync + std::fmt::Debug {
    /// Handle one inbound frame. `extension_id` is the static
    /// identity of the source microapp (matches
    /// `extensions.yaml.entries.<id>`); `line` is the original
    /// raw JSON-RPC line (`\n`-stripped). Errors are logged
    /// internally; this method never propagates failure to the
    /// reader task because admin processing must not stall the
    /// stdio loop or the regular `tools/call` flow.
    async fn handle_frame(&self, extension_id: &str, line: String);
}

/// Type alias used by `StdioSpawnOptions` and the reader task.
pub type SharedAdminRouter = Arc<dyn AdminRouter>;
