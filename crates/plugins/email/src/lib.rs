//! `nexo-plugin-email` — Email (IMAP/SMTP) channel plugin.
//!
//! Phase 48 — sub-phase 48.1 ships the scaffold + config schema only.
//! Inbound IDLE worker, SMTP outbound, MIME parse, threading, tools,
//! loop-prevention, DSN handling and SPF/DKIM checks land in 48.2..48.10.

pub mod cursor;
pub mod events;
pub mod health;
pub mod plugin;

pub use cursor::{CursorStore, UidCursor};
pub use events::InboundEvent;
pub use health::{AccountHealth, WorkerState};
pub use plugin::{EmailPlugin, TOPIC_INBOUND, TOPIC_OUTBOUND};
