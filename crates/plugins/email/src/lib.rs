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
pub mod mime_build;
pub mod mime_parse;
pub mod dsn;
pub mod loop_prevent;
pub mod outbound;
pub mod outbound_queue;
pub mod provider_hint;
pub mod spf_dkim;
pub mod threading;
pub mod plugin;
pub mod smtp_conn;
pub mod tool;

pub use cursor::{CursorStore, UidCursor};
pub use events::{AckStatus, InboundEvent, OutboundAck, OutboundCommand};
pub use health::{AccountHealth, WorkerState};
pub use mime_build::{build_mime, generate_message_id, BuildContext};
pub use mime_parse::{parse_eml, ParseConfig, ParsedMessage};
pub use threading::{
    canonicalize_message_id, enrich_reply_threading, is_self_thread, resolve_thread_root,
    session_id_for_thread, truncate_references, EMAIL_NS,
};
pub use outbound::OutboundDispatcher;
pub use outbound_queue::{OutboundJob, OutboundQueue, SmtpEnvelope};
pub use smtp_conn::{SmtpClient, SmtpSendOutcome};
pub use dsn::{parse_bounce, BounceClassification, BounceEvent, ParsedBounce};
pub use loop_prevent::{should_skip, SkipReason};
pub use provider_hint::{provider_hint, ProviderHint};
pub use spf_dkim::{check_alignment, decide_warns, parse_spf_includes, AlignmentReport};
pub use plugin::{EmailPlugin, TOPIC_INBOUND, TOPIC_OUTBOUND};
pub use tool::{
    imap_date, imap_quote, register_email_tools, run_imap_op, DispatcherHandle, EmailToolContext,
};
