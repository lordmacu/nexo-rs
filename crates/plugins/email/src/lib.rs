//! `nexo-plugin-email` — Email (IMAP/SMTP) channel plugin.
//!
//! Phase 48 — sub-phase 48.1 ships the scaffold + config schema only.
//! Inbound IDLE worker, SMTP outbound, MIME parse, threading, tools,
//! loop-prevention, DSN handling and SPF/DKIM checks land in 48.2..48.10.

pub mod cursor;
pub mod events;
pub mod health;
pub mod imap_conn;
pub mod inbound;
pub mod mime_text;
pub mod outbound;
pub mod outbound_queue;
pub mod plugin;
pub mod smtp_conn;

pub use cursor::{CursorStore, UidCursor};
pub use events::{AckStatus, InboundEvent, OutboundAck, OutboundCommand};
pub use health::{AccountHealth, WorkerState};
pub use mime_text::{build_text_mime, generate_message_id};
pub use outbound::OutboundDispatcher;
pub use outbound_queue::{OutboundJob, OutboundQueue, SmtpEnvelope};
pub use smtp_conn::{SmtpClient, SmtpSendOutcome};
pub use plugin::{EmailPlugin, TOPIC_INBOUND, TOPIC_OUTBOUND};
