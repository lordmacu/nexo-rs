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

use std::path::PathBuf;
use std::sync::Arc;

use nexo_auth::email::EmailCredentialStore;
use nexo_auth::google::GoogleCredentialStore;
use nexo_config::types::plugins::EmailPluginConfig;
use nexo_core::agent::nexo_plugin_registry::PluginFactory;
use nexo_core::agent::plugin_host::NexoPlugin;

/// Phase 81.12.d — factory builder for the email plugin. Email is a
/// **single-plugin / multi-account-internal** model: one factory call
/// returns one plugin handle that fans out across `cfg.accounts`. The
/// closure clones the four constructor arguments per call (`cfg` is
/// `Clone`; `creds` and `google` are `Arc<>` so the clone is just a
/// refcount bump; `data_dir` is a small `PathBuf`).
///
/// Today (81.12.d): exported but no caller registers it — main.rs's
/// legacy block at `src/main.rs:1914-1937` keeps constructing
/// `EmailPlugin` directly via `register_arc`. 81.12.e flips that block
/// to use this factory.
pub fn email_plugin_factory(
    cfg: EmailPluginConfig,
    creds: Arc<EmailCredentialStore>,
    google: Arc<GoogleCredentialStore>,
    data_dir: PathBuf,
) -> PluginFactory {
    Box::new(move |_manifest| {
        let plugin: Arc<dyn NexoPlugin> = Arc::new(EmailPlugin::new(
            cfg.clone(),
            creds.clone(),
            google.clone(),
            data_dir.clone(),
        ));
        Ok(plugin)
    })
}
