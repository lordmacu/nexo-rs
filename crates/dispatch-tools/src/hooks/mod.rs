//! Completion hooks.
//!
//! Provides hook types + the `HookDispatcher` trait +
//! `DefaultHookDispatcher` implementing `notify_origin`,
//! `notify_channel`, and `nats_publish`; `dispatch_phase` chaining;
//! SQLite idempotency; and the opt-in `shell` action.

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
