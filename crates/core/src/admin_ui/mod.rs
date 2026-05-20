//! Phase 99 — admin-UI contribution support (Mode A).
//!
//! Runtime side of the `[plugin.admin_ui]` manifest section: the
//! well-known slot vocabulary + trust-tier gating that the
//! `nexo/admin/plugin_ui/list` aggregator (Phase 99.5) applies
//! before exposing a plugin's contributions to the admin shell.

pub mod slot_registry;
