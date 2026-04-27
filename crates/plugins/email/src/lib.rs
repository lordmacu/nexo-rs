//! `nexo-plugin-email` — Email (IMAP/SMTP) channel plugin.
//!
//! Phase 48 — sub-phase 48.1 ships the scaffold + config schema only.
//! Inbound IDLE worker, SMTP outbound, MIME parse, threading, tools,
//! loop-prevention, DSN handling and SPF/DKIM checks land in 48.2..48.10.

pub mod attachment_store;
pub mod bounce_store;
pub mod cursor;
pub mod dsn;
pub mod events;
pub mod health;
pub mod imap_conn;
pub mod inbound;
pub mod loop_prevent;
pub mod metrics;
pub mod mime_build;
pub mod mime_parse;
pub mod outbound;
pub mod outbound_queue;
pub mod plugin;
pub mod provider_hint;
pub mod reload;
pub mod smtp_conn;
pub mod spf_dkim;
pub mod threading;
pub mod tool;

pub use attachment_store::AttachmentStore;
pub use bounce_store::{BounceStore, RecipientStatus};
pub use cursor::{CursorStore, UidCursor};
pub use dsn::{parse_bounce, BounceClassification, BounceEvent, ParsedBounce};
pub use events::{AckStatus, InboundEvent, OutboundAck, OutboundCommand};
pub use health::{AccountHealth, WorkerState};
pub use loop_prevent::{should_skip, SkipReason};
pub use mime_build::{build_mime, generate_message_id, BuildContext};
pub use mime_parse::{parse_eml, ParseConfig, ParsedMessage};
pub use outbound::OutboundDispatcher;
pub use outbound_queue::{OutboundJob, OutboundQueue, SmtpEnvelope};
pub use plugin::{EmailPlugin, TOPIC_INBOUND, TOPIC_OUTBOUND};
pub use provider_hint::{provider_hint, ProviderHint};
pub use reload::{compute_account_diff, AccountDiff};
pub use smtp_conn::{SmtpClient, SmtpSendOutcome};
pub use spf_dkim::{check_alignment, decide_warns, parse_spf_includes, AlignmentReport};
pub use threading::{
    canonicalize_message_id, enrich_reply_threading, is_self_thread, resolve_thread_root,
    session_id_for_thread, truncate_references, EMAIL_NS,
};
pub use tool::{
    filter_from_allowed_patterns, imap_date, imap_quote, register_email_tools,
    register_email_tools_filtered, run_imap_op, DispatcherHandle, EmailToolContext,
    EMAIL_TOOL_NAMES,
};
