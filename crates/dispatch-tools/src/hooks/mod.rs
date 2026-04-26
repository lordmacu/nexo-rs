//! Phase 67.F — completion hooks.
//!
//! 67.F.1 (this step): hook types + `HookDispatcher` trait +
//! `DefaultHookDispatcher` implementing `notify_origin`,
//! `notify_channel`, and `nats_publish`.
//! 67.F.2: `dispatch_phase` chaining.
//! 67.F.3: SQLite idempotency.
//! 67.F.4: opt-in `shell` action.

pub mod dispatcher;
pub mod idempotency;
pub mod registry;
pub mod types;

pub use dispatcher::{
    DefaultHookDispatcher, DispatchPhaseChainer, HookDispatcher, HookError, NatsHookPublisher,
    NoopNatsHookPublisher,
};
pub use idempotency::{HookIdempotencyStore, IdempotencyError};
pub use registry::{HookRegistry, HookRegistryStore, HookStoreError, SqliteHookRegistryStore};
pub use types::{CompletionHook, HookAction, HookPayload, HookTransition, HookTrigger};
