//! Default backend: bundles live on the local filesystem under
//! `<state_root>/tenants/<tenant>/snapshots/<agent_id>/`.
//!
//! Each public method on the [`crate::snapshotter::MemorySnapshotter`]
//! trait maps to one file in this submodule. The struct itself, its
//! builder, and the per-agent lock map live here.

mod delete;
mod diff;
mod export;
mod list;
pub mod lock;
mod restore;
mod snapshot;
mod snapshotter;
mod verify;

pub use snapshotter::{LocalFsSnapshotter, LocalFsSnapshotterBuilder};
