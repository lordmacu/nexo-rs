//! Built-in pollers shipped with the framework. To add a new one:
//!
//! 1. Create `crates/poller/src/builtins/<your_kind>.rs` with a struct
//!    that `impl Poller`.
//! 2. Add a `pub mod` line below.
//! 3. Push your struct into [`register_all`].
//!
//! That is the only place wiring is touched — `main.rs` calls
//! `register_all` once at boot. See `docs/src/recipes/build-a-poller.md`
//! for the full pattern.

// Step 17 of the plan registers the four V1 built-ins here.
// Stubs for now so the crate compiles before the runner exists.

#[allow(dead_code)]
pub fn register_all_placeholder() {
    // No-op until step 17 wires the registry.
}
